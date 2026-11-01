//! Generyczne przekazywanie poleceń Vulkana do serwera.
//!
//! `entry::<OP>` to odwrotność thunka serwera: jeden odbiornik czyta argumenty całkowite z rejestrów i stosu oraz
//! `float`/`double` z `xmm` (System V x86_64), więc obsługuje dowolne polecenie bez ręcznej sygnatury. Dalej:
//! parametry trafiają do 8-bajtowych slotów, kodek zamienia je na bajty, a klasa polecenia (z `vk.xml`) wybiera
//! drogę: partia bez czekania (większość) albo synchroniczne zapytanie (polecenia z wynikiem lub oczekiwaniem).

use std::collections::HashMap;

use gpu_proto::meta::{CmdClass, Elem, Ret, Shape, COMMANDS};
use gpu_proto::msg::{Request, Response, MAX_BATCH};
use gpu_vkcodec::codec::{decode_out_params, encode_command, out_handles, Ctx};
use gpu_vkcodec::CodecError;
use gpu_wire::{Decoder, Encoder};

use super::{lock, InstState, InstanceObj, CURRENT};

pub type Entry = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, f64, f64, f64, f64, f64, f64, f64, f64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;

const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_MEMORY_MAP_FAILED: i32 = -5;
const VK_ERROR_DEVICE_LOST: i32 = -4;
/// Partia wychodzi, gdy przekroczy ten rozmiar (albo na punkcie synchronizacji).
const BATCH_FLUSH: usize = 64 * 1024;
const MEM_CHUNK: usize = 4 * 1024 * 1024;

const DISPATCHABLE: &[&str] = &["VkInstance", "VkPhysicalDevice", "VkDevice", "VkQueue", "VkCommandBuffer"];

/// Obiekt dyspozycyjny ICD (urządzenie, kolejka, bufor poleceń): wskaźnik loadera i identyfikator protokołu.
#[repr(C)]
pub struct DispObj {
    loader_data: usize,
    id: u64,
}

/// Liczniki ruchu do pomiaru bazowego (M2): ile bajtów i obiegów kosztuje naiwne przekazywanie.
#[derive(Default, Debug)]
pub struct Stats {
    pub cmd_bytes: u64,
    pub cmd_batches: u64,
    pub memw_bytes: u64,
    pub memr_bytes: u64,
    pub query_bytes: u64,
    pub roundtrips: u64,
    pub commands: u64,
}

impl Stats {
    pub fn report(&self) -> String {
        format!(
            "gpu-icd statystyki: poleceń {}, partii {}, bajty poleceń {}, zapisy pamięci {} B, odczyty pamięci {} B, zapytania {} B, obiegi po sieci {}",
            self.commands, self.cmd_batches, self.cmd_bytes, self.memw_bytes, self.memr_bytes, self.query_bytes, self.roundtrips
        )
    }
}

/// Lokalna kopia odwzorowanej pamięci (M2a: całe obiekty, M3: tylko zmienione strony).
pub struct Shadow {
    data: Vec<u8>,
    mapped: bool,
}

/// Tłumaczy uchwyty dyspozycyjne (wskaźnik na obiekt -> identyfikator pod offsetem 8); reszta to identyfikatory.
struct IcdCtx;

impl Ctx for IcdCtx {
    fn handle_to_wire(&self, ty: &str, raw: u64) -> Result<u64, CodecError> {
        if raw != 0 && DISPATCHABLE.contains(&ty) {
            // SAFETY: uchwyty dyspozycyjne to wskaźniki na obiekty ICD z identyfikatorem pod offsetem 8.
            Ok(unsafe { std::ptr::read_unaligned((raw as *const u8).add(8) as *const u64) })
        } else {
            Ok(raw)
        }
    }
    fn wire_to_handle(&self, _ty: &str, wire: u64) -> Result<u64, CodecError> {
        Err(CodecError::Handle(wire))
    }
}

impl InstState {
    pub(crate) fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn lose(&mut self) {
        self.device_lost = true;
    }

    /// Wysyła zgromadzoną partię poleceń (bez czekania na odpowiedź).
    pub(crate) fn flush(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.batch);
        self.stats.cmd_bytes += data.len() as u64;
        self.stats.cmd_batches += 1;
        if self.client.send(&Request::Cmds { data }).is_err() {
            self.lose();
        }
    }

    fn send_mem(&mut self, memory: u64, offset: u64, data: &[u8]) {
        self.flush();
        self.stats.memw_bytes += data.len() as u64;
        for (i, chunk) in data.chunks(MEM_CHUNK.min(MAX_BATCH)).enumerate() {
            let req = Request::MemWrite { memory, offset: offset + (i * MEM_CHUNK) as u64, data: chunk.to_vec() };
            if self.client.send(&req).is_err() {
                self.lose();
            }
        }
    }

    /// Przed `vkQueueSubmit`: serwer musi zobaczyć wszystko, co aplikacja wpisała w odwzorowaną pamięć.
    fn write_mapped(&mut self) {
        let ids: Vec<u64> = self.mems.iter().filter(|(_, s)| s.mapped).map(|(i, _)| *i).collect();
        for id in ids {
            let Some((p, l)) = self.mems.get(&id).map(|s| (s.data.as_ptr(), s.data.len())) else { continue };
            // SAFETY: bufor kopii nie zmienia adresu ani rozmiaru po utworzeniu; wskaźnik nie pożycza `self`.
            let data = unsafe { std::slice::from_raw_parts(p, l) };
            self.send_mem(id, 0, data);
        }
    }

    pub(crate) fn read_mem(&mut self, memory: u64, offset: u64, len: usize) -> Option<Vec<u8>> {
        self.flush();
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = (len - out.len()).min(MEM_CHUNK);
            self.stats.roundtrips += 1;
            self.stats.memr_bytes += n as u64;
            match self.client.call(&Request::MemRead { memory, offset: offset + out.len() as u64, len: n as u32 }) {
                Ok(Response::Data(d)) if d.len() == n => out.extend(d),
                _ => {
                    self.lose();
                    return None;
                }
            }
        }
        Some(out)
    }

    /// Po oczekiwaniu (fence/idle): GPU mogło zapisać odwzorowaną pamięć, więc odświeżamy lokalne kopie.
    fn read_mapped(&mut self) {
        let ids: Vec<(u64, usize)> = self.mems.iter().filter(|(_, s)| s.mapped).map(|(i, s)| (*i, s.data.len())).collect();
        for (id, len) in ids {
            if let Some(d) = self.read_mem(id, 0, len) {
                // Kopiujemy do istniejącego bufora: aplikacja trzyma wskaźnik z `vkMapMemory`.
                if let Some(s) = self.mems.get_mut(&id) {
                    s.data.copy_from_slice(&d);
                }
            }
        }
    }
}

fn state() -> Option<&'static InstanceObj> {
    // SAFETY: wskaźnik ustawia `create_instance` i zeruje `destroy_instance`; obiekt żyje między nimi.
    unsafe { CURRENT.load(std::sync::atomic::Ordering::SeqCst).as_ref() }
}

/// Punkt wejścia ICD dla polecenia o kodzie `OP` (indeks w `COMMANDS`).
///
/// # Safety
/// Wołany przez aplikację/loader z argumentami zgodnymi z sygnaturą polecenia z `vk.xml`.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn entry<const OP: u16>(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, x0: f64, x1: f64, x2: f64, x3: f64, x4: f64, x5: f64, x6: f64, x7: f64, s0: u64, s1: u64, s2: u64, s3: u64, s4: u64, s5: u64, s6: u64, s7: u64) -> u64 {
    forward(OP, [a0, a1, a2, a3, a4, a5, s0, s1, s2, s3, s4, s5, s6, s7], [x0, x1, x2, x3, x4, x5, x6, x7])
}

unsafe fn forward(op: u16, ints: [u64; 14], floats: [f64; 8]) -> u64 {
    let Some(cmd) = COMMANDS.get(op as usize) else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    let mut st = lock(&inst.state);
    let fail = |e: i32| -> u64 { e as u32 as u64 };
    if st.device_lost && cmd.ret == Ret::VkResult {
        return fail(VK_ERROR_DEVICE_LOST);
    }
    // Argumenty -> sloty (odwrotność thunka serwera).
    let params = cmd.params();
    let mut slots = vec![0u64; params.len()];
    let (mut ii, mut fi) = (0, 0);
    for (k, p) in params.iter().enumerate() {
        slots[k] = match (p.shape, p.elem, p.type_name) {
            (Shape::Value, Elem::Scalar, "float") => {
                fi += 1;
                floats.get(fi - 1).map_or(0, |f| f.to_bits() & 0xFFFF_FFFF)
            }
            (Shape::Value, Elem::Scalar, "double") => {
                fi += 1;
                floats.get(fi - 1).map_or(0, |f| f.to_bits())
            }
            _ => {
                ii += 1;
                ints.get(ii - 1).copied().unwrap_or(0)
            }
        };
    }
    exec(&mut st, op, &mut slots)
}

/// Wykonuje polecenie o kodzie `op` z gotowymi slotami parametrów (tak samo dla aplikacji i dla ICD, które wydaje własne polecenia).
///
/// # Safety
/// Sloty muszą zawierać poprawne wartości parametrów polecenia (wskaźniki do żywej pamięci).
pub(crate) unsafe fn exec(st: &mut InstState, op: u16, slots: &mut [u64]) -> u64 {
    let Some(cmd) = COMMANDS.get(op as usize) else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    let params = cmd.params();
    let fail = |e: i32| -> u64 { e as u32 as u64 };
    let args = slots.as_ptr() as *const u8;

    // Kolejka wydana już wcześniej (to samo urządzenie, rodzina, indeks) wraca bez wysyłania.
    if cmd.name == "vkGetDeviceQueue" {
        let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
        if let Some(q) = st.queues.get(&(dev, slots[1] as u32, slots[2] as u32)) {
            std::ptr::write_unaligned(slots[3] as *mut usize, *q);
            return 0;
        }
    }
    // Uchwyty wyjściowe nadaje klient: wpisujemy identyfikatory w pamięć aplikacji przed zakodowaniem.
    let mut disp_outs: Vec<(*mut u64, u64)> = Vec::new();
    let Ok(outs) = out_handles(cmd, args) else { return fail(VK_ERROR_INITIALIZATION_FAILED) };
    for (kind, ptr, n) in outs {
        for i in 0..n {
            let id = st.alloc_id();
            std::ptr::write_unaligned(ptr.add(i), id);
            if DISPATCHABLE.contains(&kind) {
                disp_outs.push((ptr.add(i), id));
            }
        }
    }
    if cmd.name.starts_with("vkQueueSubmit") {
        st.write_mapped();
    }
    let mut e = Encoder::new();
    if encode_command(op, args, &mut e, &IcdCtx).is_err() {
        return fail(VK_ERROR_INITIALIZATION_FAILED);
    }
    let bytes = e.finish();
    if cmd.name == "vkAllocateMemory" {
        if let Some(size) = gpu_vkcodec::codec::arg_uint(cmd, args, "pAllocateInfo->allocationSize") {
            let id = disp_outs.first().map(|x| x.1).unwrap_or(0);
            let mem_id = std::ptr::read_unaligned(slots[3] as *const u64);
            let _ = id;
            st.mem_sizes.insert(mem_id, size);
        }
    }
    if cmd.name == "vkCreateDevice" {
        if let (Some((_, dev_id)), true) = (disp_outs.first(), slots[0] != 0) {
            // PdevObj: identyfikator zdalnej karty pod offsetem 8.
            st.device_pdev.insert(*dev_id, std::ptr::read_unaligned((slots[0] as *const u8).add(8) as *const u64));
        }
    }
    // Obiekty dyspozycyjne: aplikacja dostaje wskaźnik na obiekt ICD, a w protokole obowiązuje jego identyfikator.
    for (slot, id) in disp_outs {
        let obj = Box::into_raw(Box::new(DispObj { loader_data: 0, id }));
        std::ptr::write_unaligned(slot, obj as u64);
        if cmd.name == "vkGetDeviceQueue" {
            let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
            st.queues.insert((dev, slots[1] as u32, slots[2] as u32), obj as usize);
        }
    }
    // Zwolnienie obiektów ICD po zniszczeniu (identyfikator został już zakodowany).
    if cmd.name == "vkDestroyDevice" && slots[0] != 0 {
        drop(Box::from_raw(slots[0] as *mut DispObj));
    }
    if cmd.name == "vkFreeCommandBuffers" {
        for i in 0..slots[2] as usize {
            let p = std::ptr::read_unaligned((slots[3] as *const u64).add(i));
            if p != 0 {
                drop(Box::from_raw(p as *mut DispObj));
            }
        }
    }
    if cmd.name == "vkFreeMemory" {
        st.mems.remove(&slots[1]);
        st.mem_sizes.remove(&slots[1]);
    }

    st.stats.commands += 1;
    let has_data_out = params.iter().any(|p| p.shape == Shape::Pointer && !p.is_const && p.elem != Elem::Handle && p.ptr_depth == 1);
    if cmd.class != CmdClass::Sync && !has_data_out {
        st.batch.extend(bytes);
        if st.batch.len() > BATCH_FLUSH {
            st.flush();
        }
        return 0;
    }
    // Droga synchroniczna: partia, potem polecenie, potem `Sync` (błąd asynchroniczny); jeden obieg po sieci.
    st.flush();
    st.stats.query_bytes += bytes.len() as u64;
    st.stats.roundtrips += 1;
    let ok = st.client.send(&Request::Query { data: bytes }).is_ok() && st.client.send(&Request::Sync).is_ok();
    let (q, sy) = if ok { (st.client.recv(), st.client.recv()) } else { (Err(gpu_renderd::client::ClientError::Closed), Err(gpu_renderd::client::ClientError::Closed)) };
    if let Ok(Response::Synced { error }) = sy {
        if error < 0 {
            st.lose();
        }
    }
    let Ok(Response::Data(d)) = q else {
        st.lose();
        return fail(VK_ERROR_DEVICE_LOST);
    };
    let result = d.get(..4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(VK_ERROR_DEVICE_LOST);
    if result >= 0 && decode_out_params(cmd, args, &mut Decoder::new(d.get(4..).unwrap_or(&[]))).is_err() {
        return fail(VK_ERROR_INITIALIZATION_FAILED);
    }
    if cmd.class == CmdClass::Sync {
        st.read_mapped();
    }
    fail(result)
}

// ---------- pamięć odwzorowana (lokalnie; synchronizacja z serwerem w punktach) ----------

pub unsafe extern "C" fn map_memory(_dev: u64, mem: u64, offset: u64, size: u64, _flags: u32, out: *mut *mut u8) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut st = lock(&inst.state);
    let Some(total) = st.mem_sizes.get(&mem).copied() else { return VK_ERROR_MEMORY_MAP_FAILED };
    if offset > total || (size != u64::MAX && offset.saturating_add(size) > total) || out.is_null() {
        return VK_ERROR_MEMORY_MAP_FAILED;
    }
    let sh = st.mems.entry(mem).or_insert_with(|| Shadow { data: vec![0u8; total as usize], mapped: false });
    sh.mapped = true;
    *out = sh.data.as_mut_ptr().add(offset as usize);
    0
}

pub unsafe extern "C" fn unmap_memory(_dev: u64, mem: u64) {
    if let Some(inst) = state() {
        let mut st = lock(&inst.state);
        let span = st.mems.get_mut(&mem).map(|s| {
            s.mapped = false;
            (s.data.as_ptr(), s.data.len())
        });
        // Zapis z odwzorowania musi dotrzeć na serwer, bo po Unmap nie będzie już śledzony.
        if let Some((p, l)) = span {
            st.send_mem(mem, 0, std::slice::from_raw_parts(p, l));
        }
    }
}

unsafe fn ranges(count: u32, r: *const u8) -> Vec<(u64, u64, u64)> {
    // VkMappedMemoryRange (40 B): memory @16, offset @24, size @32.
    (0..count as usize)
        .map(|i| {
            let b = r.add(i * 40);
            (std::ptr::read_unaligned(b.add(16) as *const u64), std::ptr::read_unaligned(b.add(24) as *const u64), std::ptr::read_unaligned(b.add(32) as *const u64))
        })
        .collect()
}

pub unsafe extern "C" fn flush_ranges(_dev: u64, count: u32, r: *const u8) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut st = lock(&inst.state);
    for (mem, off, size) in ranges(count, r) {
        let Some(sh) = st.mems.get(&mem) else { continue };
        let end = if size == u64::MAX { sh.data.len() as u64 } else { off.saturating_add(size).min(sh.data.len() as u64) };
        let part = sh.data.get(off as usize..end as usize).map(|s| s.to_vec()).unwrap_or_default();
        st.send_mem(mem, off, &part);
    }
    0
}

pub unsafe extern "C" fn invalidate_ranges(_dev: u64, count: u32, r: *const u8) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut st = lock(&inst.state);
    let mut map: HashMap<u64, ()> = HashMap::new();
    for (mem, off, size) in ranges(count, r) {
        let Some(len) = st.mems.get(&mem).map(|s| s.data.len() as u64) else { continue };
        let end = if size == u64::MAX { len } else { off.saturating_add(size).min(len) };
        if let Some(d) = st.read_mem(mem, off, (end - off) as usize) {
            if let Some(s) = st.mems.get_mut(&mem) {
                s.data[off as usize..end as usize].copy_from_slice(&d);
            }
        }
        map.insert(mem, ());
    }
    0
}

/// Wykonuje polecenie po nazwie (dla kodu ICD wydającego własne polecenia, np. swapchain). Zwraca kod wyniku.
///
/// # Safety
/// Jak `exec`.
pub(crate) unsafe fn exec_named(st: &mut InstState, name: &str, slots: &mut [u64]) -> i32 {
    match gpu_proto::meta::find_command(name) {
        Some((op, c)) if c.params().len() == slots.len() => exec(st, op, slots) as u32 as i32,
        _ => VK_ERROR_INITIALIZATION_FAILED,
    }
}

impl InstState {
    /// Właściwości pamięci zdalnej karty (`MemoryProperties2`, z pamięci podręcznej).
    pub(crate) fn query_props(&mut self, pdev: u64) -> Option<Vec<u8>> {
        self.query(pdev, gpu_proto::msg::PdFunc::MemoryProperties2, 0, 0, &[])
    }
}
