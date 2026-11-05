//! Odczyt pamięci urządzenia przez kopię GPU. Pamięć DEVICE_LOCAL|HOST_VISIBLE (okno BAR) czyta procesor z prędkością ok. 400 MB/s, a po
//! każdym oczekiwaniu klient odświeża z niej megabajty (Black Book: 199 s z 283 s czekania). Kopia przez GPU do zwykłej pamięci RAM
//! (HOST_CACHED) idzie z prędkością magistrali PCIe, a porównanie stron czyta już szybką pamięć. Dane są te same co przy odczycie procesorem
//! (po oczekiwaniu klienta na GPU); każda porażka wraca do odczytu procesorem.

use std::collections::HashMap;

use super::{call_thunk, Replayer, Staged, VK_WHOLE_SIZE};
use crate::handler::ERR_UNKNOWN;

const BUFFER_USAGE_TRANSFER_SRC: u32 = 0x1;
const BUFFER_USAGE_TRANSFER_DST: u32 = 0x2;
const MEM_HOST_VISIBLE: u32 = 0x2;
const MEM_HOST_COHERENT: u32 = 0x4;
const MEM_HOST_CACHED: u32 = 0x8;
/// Poniżej tej sumy bajtów odczyt procesorem jest tańszy niż zgłoszenie kopii i czekanie na płotek.
pub(super) const MIN_BYTES: usize = 256 * 1024;
const MAX_REGIONS: usize = 256;

#[derive(Default)]
pub(super) struct Staging {
    buf: u64,
    mem: u64,
    ptr: usize,
    size: usize,
    coherent: bool,
}

#[derive(Default)]
struct DevCopy {
    pool: u64,
    cmd: u64,
    fence: u64,
    staging: Staging,
    /// Pamięć (prawdziwy uchwyt) -> bufor TRANSFER_SRC związany z całą pamięcią.
    srcs: HashMap<u64, u64>,
}

#[derive(Default)]
pub(super) struct GpuCopy {
    /// Prawdziwe urządzenie -> (prawdziwa kolejka, rodzina) z pierwszego `vkGetDeviceQueue`.
    pub queues: HashMap<u64, (u64, u32)>,
    devs: HashMap<u64, DevCopy>,
    /// Identyfikator pamięci klienta -> indeks typu pamięci z `vkAllocateMemory`.
    pub mem_types: HashMap<u64, u32>,
    failures: u32,
    pub used: u64,
    pub ns: u64,
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

impl Replayer {
    fn vk(&mut self, name: &str, ints: &[u64]) -> Result<i32, i32> {
        let f = self.func_by_name(name)?;
        // SAFETY: nazwa i liczba parametrów odpowiadają deklaracji polecenia; wskaźniki w `ints` żyją przez czas wywołania.
        let r = unsafe { call_thunk(f, ints, &[]) }.ok_or(ERR_UNKNOWN)?;
        Ok(r as u32 as i32)
    }

    fn vk_ok(&mut self, name: &str, ints: &[u64]) -> Result<(), i32> {
        let r = self.vk(name, ints)?;
        if r < 0 {
            eprintln!("replay: kopia GPU: {name} zwróciło {r}");
            return Err(r);
        }
        Ok(())
    }

    /// Typ pamięci hosta z HOST_VISIBLE|HOST_CACHED (najlepiej też COHERENT) dla bufora o masce `bits`: `(indeks, coherent)`.
    fn host_cached_type(&mut self, physical: u64, bits: u32) -> Result<(u32, bool), i32> {
        let mut props = [0u8; 520];
        self.vk("vkGetPhysicalDeviceMemoryProperties", &[physical, props.as_mut_ptr() as u64])?;
        let n = u32::from_le_bytes(props[0..4].try_into().unwrap()).min(32) as usize;
        let mut best: Option<(u32, bool)> = None;
        for i in 0..n {
            let fl = u32::from_le_bytes(props[4 + 8 * i..8 + 8 * i].try_into().unwrap());
            if bits & (1 << i) == 0 || fl & (MEM_HOST_VISIBLE | MEM_HOST_CACHED) != (MEM_HOST_VISIBLE | MEM_HOST_CACHED) {
                continue;
            }
            let coherent = fl & MEM_HOST_COHERENT != 0;
            if best.is_none() || coherent {
                best = Some((i as u32, coherent));
            }
        }
        best.ok_or(ERR_UNKNOWN)
    }

    fn create_buffer(&mut self, device: u64, size: u64, usage: u32) -> Result<(u64, u64, u32), i32> {
        let mut ci = [0u8; 56];
        put32(&mut ci, 0, 12);
        put64(&mut ci, 24, size);
        put32(&mut ci, 32, usage);
        let mut buf = 0u64;
        self.vk_ok("vkCreateBuffer", &[device, ci.as_ptr() as u64, 0, &mut buf as *mut u64 as u64])?;
        let mut req = [0u8; 24];
        self.vk("vkGetBufferMemoryRequirements", &[device, buf, req.as_mut_ptr() as u64])?;
        Ok((buf, u64::from_le_bytes(req[0..8].try_into().unwrap()), u32::from_le_bytes(req[16..20].try_into().unwrap())))
    }

    fn destroy_dev_copy(&mut self, device: u64, d: DevCopy) {
        let _ = self.vk("vkDeviceWaitIdle", &[device]);
        for (_, b) in d.srcs {
            let _ = self.vk("vkDestroyBuffer", &[device, b, 0]);
        }
        if d.staging.buf != 0 {
            let _ = self.vk("vkDestroyBuffer", &[device, d.staging.buf, 0]);
            let _ = self.vk("vkFreeMemory", &[device, d.staging.mem, 0]);
        }
        if d.fence != 0 {
            let _ = self.vk("vkDestroyFence", &[device, d.fence, 0]);
        }
        if d.pool != 0 {
            let _ = self.vk("vkDestroyCommandPool", &[device, d.pool, 0]);
        }
    }

    /// Przed `vkDestroyDevice`: zwalnia obiekty kopii tego urządzenia (sterownik NVIDIA nie zwalnia pamięci przy niszczeniu urządzenia).
    pub(super) fn copy_forget_device(&mut self, device: u64) {
        self.copy.queues.remove(&device);
        if let Some(d) = self.copy.devs.remove(&device) {
            self.destroy_dev_copy(device, d);
        }
    }

    /// Przed `vkFreeMemory`: zdejmuje bufor, który kopia związała z tą pamięcią.
    pub(super) fn copy_forget_memory(&mut self, device: u64, memory: u64) {
        let b = self.copy.devs.get_mut(&device).and_then(|d| d.srcs.remove(&memory));
        if let Some(b) = b {
            let _ = self.vk("vkDestroyBuffer", &[device, b, 0]);
        }
    }

    fn ensure_dev(&mut self, device: u64) -> Result<(), i32> {
        if self.copy.devs.get(&device).is_some_and(|d| d.fence != 0) {
            return Ok(());
        }
        let &(_, family) = self.copy.queues.get(&device).ok_or(ERR_UNKNOWN)?;
        let mut d = DevCopy::default();
        let mut pi = [0u8; 24];
        put32(&mut pi, 0, 39);
        put32(&mut pi, 16, 2); // RESET_COMMAND_BUFFER
        put32(&mut pi, 20, family);
        self.vk_ok("vkCreateCommandPool", &[device, pi.as_ptr() as u64, 0, &mut d.pool as *mut u64 as u64])?;
        let mut ai = [0u8; 32];
        put32(&mut ai, 0, 40);
        put64(&mut ai, 16, d.pool);
        put32(&mut ai, 28, 1);
        self.vk_ok("vkAllocateCommandBuffers", &[device, ai.as_ptr() as u64, &mut d.cmd as *mut u64 as u64])?;
        let mut fi = [0u8; 24];
        put32(&mut fi, 0, 8);
        self.vk_ok("vkCreateFence", &[device, fi.as_ptr() as u64, 0, &mut d.fence as *mut u64 as u64])?;
        self.copy.devs.insert(device, d);
        Ok(())
    }

    fn ensure_staging(&mut self, device: u64, physical: u64, size: usize) -> Result<(), i32> {
        if self.copy.devs.get(&device).is_some_and(|d| d.staging.size >= size) {
            return Ok(());
        }
        if let Some(old) = self.copy.devs.get_mut(&device).map(|d| std::mem::take(&mut d.staging)).filter(|s| s.buf != 0) {
            let _ = self.vk("vkDestroyBuffer", &[device, old.buf, 0]);
            let _ = self.vk("vkFreeMemory", &[device, old.mem, 0]);
        }
        let (buf, req_size, bits) = self.create_buffer(device, size as u64, BUFFER_USAGE_TRANSFER_DST)?;
        let (ty, coherent) = self.host_cached_type(physical, bits)?;
        let mut ai = [0u8; 32];
        put32(&mut ai, 0, 5);
        put64(&mut ai, 16, req_size);
        put32(&mut ai, 24, ty);
        let mut mem = 0u64;
        self.vk_ok("vkAllocateMemory", &[device, ai.as_ptr() as u64, 0, &mut mem as *mut u64 as u64])?;
        self.vk_ok("vkBindBufferMemory", &[device, buf, mem, 0])?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        self.vk_ok("vkMapMemory", &[device, mem, 0, VK_WHOLE_SIZE, 0, &mut ptr as *mut *mut u8 as u64])?;
        if let Some(d) = self.copy.devs.get_mut(&device) {
            d.staging = Staging { buf, mem, ptr: ptr as usize, size, coherent };
        }
        Ok(())
    }

    fn ensure_src(&mut self, device: u64, memory: u64, mem_type: u32, size: usize) -> Result<u64, i32> {
        if let Some(&b) = self.copy.devs.get(&device).and_then(|d| d.srcs.get(&memory)) {
            return Ok(b);
        }
        let (buf, req_size, bits) = self.create_buffer(device, size as u64, BUFFER_USAGE_TRANSFER_SRC)?;
        if bits & (1 << mem_type) == 0 || req_size > size as u64 {
            let _ = self.vk("vkDestroyBuffer", &[device, buf, 0]);
            return Err(ERR_UNKNOWN);
        }
        if let Err(e) = self.vk_ok("vkBindBufferMemory", &[device, buf, memory, 0]) {
            let _ = self.vk("vkDestroyBuffer", &[device, buf, 0]);
            return Err(e);
        }
        if let Some(d) = self.copy.devs.get_mut(&device) {
            d.srcs.insert(memory, buf);
        }
        Ok(buf)
    }

    /// Kopiuje `spans` pamięci `memory` (identyfikator klienta) przez GPU do bufora w RAM pod tymi samymi przesunięciami; zwraca wskaźnik na lustro.
    /// `Err`: nie da się (brak kolejki, niezgodny typ pamięci, błąd sterownika) i trzeba czytać procesorem.
    fn copy_via_gpu(&mut self, memory: u64, spans: &[(usize, usize)]) -> Result<Staged, i32> {
        if self.copy.failures >= 3 {
            return Err(ERR_UNKNOWN);
        }
        let e = *self.registry.get(memory).ok_or(ERR_UNKNOWN)?;
        let (device, mem_real, size) = (e.parent, e.real, e.size as usize);
        let physical = self.registry.entries_of_kind("VkDevice").into_iter().find(|(_, d)| d.real == device).map(|(_, d)| d.parent).ok_or(ERR_UNKNOWN)?;
        let mem_type = *self.copy.mem_types.get(&memory).ok_or(ERR_UNKNOWN)?;
        let &(queue, _) = self.copy.queues.get(&device).ok_or(ERR_UNKNOWN)?;
        let r = (|| {
            self.ensure_dev(device)?;
            self.ensure_staging(device, physical, size)?;
            let src = self.ensure_src(device, mem_real, mem_type, size)?;
            let d = self.copy.devs.get(&device).ok_or(ERR_UNKNOWN)?;
            let (cmd, fence, dst, stg_ptr, coherent, stg_mem) = (d.cmd, d.fence, d.staging.buf, d.staging.ptr, d.staging.coherent, d.staging.mem);
            let mut regions: Vec<u8> = Vec::new();
            let mut spans_v: Vec<(usize, usize)> = spans.to_vec();
            if spans_v.len() > MAX_REGIONS {
                spans_v = vec![(spans_v[0].0, spans_v[spans_v.len() - 1].1)];
            }
            for &(a, b) in &spans_v {
                for v in [a as u64, a as u64, (b - a) as u64] {
                    regions.extend_from_slice(&v.to_le_bytes());
                }
            }
            let mut bi = [0u8; 32];
            put32(&mut bi, 0, 42);
            put32(&mut bi, 16, 1); // ONE_TIME_SUBMIT
            self.vk_ok("vkResetCommandBuffer", &[cmd, 0])?;
            self.vk_ok("vkBeginCommandBuffer", &[cmd, bi.as_ptr() as u64])?;
            let mut mb = [0u8; 24];
            put32(&mut mb, 0, 46);
            put32(&mut mb, 16, 0x10000); // MEMORY_WRITE
            put32(&mut mb, 20, 0x800); // TRANSFER_READ
            self.vk("vkCmdPipelineBarrier", &[cmd, 0x10000, 0x1000, 0, 1, mb.as_ptr() as u64, 0, 0, 0, 0])?;
            self.vk("vkCmdCopyBuffer", &[cmd, src, dst, spans_v.len() as u64, regions.as_ptr() as u64])?;
            self.vk_ok("vkEndCommandBuffer", &[cmd])?;
            let mut si = [0u8; 72];
            put32(&mut si, 0, 4);
            put32(&mut si, 40, 1);
            put64(&mut si, 48, &cmd as *const u64 as u64);
            self.vk_ok("vkQueueSubmit", &[queue, 1, si.as_ptr() as u64, fence])?;
            let w = self.vk("vkWaitForFences", &[device, 1, &fence as *const u64 as u64, 1, 2_000_000_000])?;
            self.vk_ok("vkResetFences", &[device, 1, &fence as *const u64 as u64]).ok();
            if w != 0 {
                eprintln!("replay: kopia GPU: czekanie na płotek zwróciło {w}");
                return Err(ERR_UNKNOWN);
            }
            if !coherent {
                let mut r = [0u8; 40];
                put32(&mut r, 0, 6);
                put64(&mut r, 16, stg_mem);
                put64(&mut r, 32, VK_WHOLE_SIZE);
                self.vk_ok("vkInvalidateMappedMemoryRanges", &[device, 1, r.as_ptr() as u64])?;
            }
            Ok(Staged::Mapped(stg_ptr as *const u8, size))
        })();
        match &r {
            Ok(_) => self.copy.used += 1,
            Err(_) => self.copy.failures += 1,
        }
        r
    }

    /// Dane do porównania stron `spans` z pamięci `memory`: kopia GPU, gdy się opłaca i się da; `None` = czytaj procesorem.
    pub(super) fn gpu_staged(&mut self, memory: u64, spans: &[(usize, usize)]) -> Option<Staged> {
        let total: usize = spans.iter().map(|(a, b)| b - a).sum();
        if total < MIN_BYTES || std::env::var_os("GPU_RENDERD_NO_GPU_COPY").is_some() {
            return None;
        }
        let t = std::time::Instant::now();
        let r = self.copy_via_gpu(memory, spans).ok();
        self.copy.ns += t.elapsed().as_nanos() as u64;
        if self.copy.used % 2000 == 1 && r.is_some() {
            eprintln!("gpu-renderd: kopia GPU odczytu pamięci: {} użyć, {} ms łącznie, {} porażek", self.copy.used, self.copy.ns / 1_000_000, self.copy.failures);
        }
        r
    }
}
