//! `gpu-smoke`: aplikacja Vulkana przechodząca przez loader (nasz ICD przez `VK_DRIVER_FILES`):
//! wypełnia bufor na zdalnej karcie i odczytuje wynik przez zmapowaną pamięć.
//!
//! Użycie: GPU_ICD_ADDR=... GPU_ICD_TOKEN_FILE=... VK_DRIVER_FILES=... gpu-smoke [liczba-powtórzeń]

use std::ffi::{c_void, CStr};
use std::time::Instant;

use gpu_proto::meta::{find_struct, StructMeta};
use gpu_proto::stype::STYPE_TABLE;
use gpu_renderd::vk::loader::{GlobalFns, InstanceFns, LibVulkan};
use gpu_renderd::vk::replay::call_thunk;

struct S {
    meta: &'static StructMeta,
    w: Vec<u64>,
}

impl S {
    fn new(name: &str) -> S {
        let meta = find_struct(name).unwrap();
        let mut s = S { meta, w: vec![0; (meta.size as usize).div_ceil(8).max(1)] };
        if let Some(e) = STYPE_TABLE.iter().find(|e| e.name == name) {
            s.u32("sType", e.stype);
        }
        s
    }
    fn off(&self, m: &str) -> usize {
        self.meta.members().iter().find(|x| x.name == m).unwrap().offset as usize
    }
    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: bufor `u64` jako bajty, długość 8 * liczba słów.
        unsafe { std::slice::from_raw_parts_mut(self.w.as_mut_ptr() as *mut u8, self.w.len() * 8) }
    }
    fn u32(&mut self, m: &str, v: u32) -> &mut S {
        let o = self.off(m);
        self.bytes()[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, m: &str, v: u64) -> &mut S {
        let o = self.off(m);
        self.bytes()[o..o + 8].copy_from_slice(&v.to_le_bytes());
        self
    }
    fn p(&self) -> u64 {
        self.w.as_ptr() as u64
    }
}

fn die(msg: &str) -> ! {
    eprintln!("BŁĄD: {msg}");
    std::process::exit(1);
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

const SIZE: u64 = 4096;

fn main() {
    let reps: usize = std::env::args().nth(1).and_then(|v| v.parse().ok()).unwrap_or(1);
    let lib = LibVulkan::load().unwrap_or_else(|e| die(&format!("libvulkan: {e:?}")));
    let g = GlobalFns::load(&lib).unwrap_or_else(|e| die(&format!("funkcje globalne: {e:?}")));
    let t0 = Instant::now();
    let mut info = [0u64; 8];
    info[0] = 1;
    let mut inst: *mut c_void = std::ptr::null_mut();
    // SAFETY: poprawne struktury i wskaźniki wyjściowe.
    let r = unsafe { (g.create_instance)(info.as_ptr() as *const c_void, std::ptr::null(), &mut inst) };
    if r != 0 {
        die(&format!("vkCreateInstance = {r} (czy GPU_ICD_ADDR i token są ustawione, a demon działa?)"));
    }
    let f = unsafe { InstanceFns::load(&lib, inst) }.unwrap_or_else(|e| die(&format!("funkcje instancji: {e:?}")));
    let mut n = 0u32;
    let mut pds = vec![std::ptr::null_mut(); 8];
    n = n.max(8);
    unsafe { (f.enumerate_physical_devices)(inst, &mut n, pds.as_mut_ptr()) };
    if n == 0 {
        die("brak urządzeń");
    }
    let pd = pds[0];
    let mut props = vec![0u64; 105];
    props[0] = 1_000_059_001;
    unsafe { (f.get_properties2)(pd, props.as_mut_ptr() as *mut c_void) };
    let pb = unsafe { std::slice::from_raw_parts(props.as_ptr() as *const u8, 840) };
    println!("urządzenie: {}", CStr::from_bytes_until_nul(&pb[36..36 + 256]).unwrap().to_string_lossy());

    // Funkcje wskazywane przez loader (poleceń urządzenia nie ma w InstanceFns): thunk z tablicą slotów.
    let func = |name: &str| -> *const c_void {
        let c = std::ffi::CString::new(name).unwrap();
        // SAFETY: instancja jest ważna.
        unsafe { lib.proc_addr(inst, &c) }.unwrap_or_else(|| die(&format!("loader nie zna {name}")))
    };
    let call = |name: &str, ints: &[u64]| -> i32 {
        // SAFETY: wskaźniki w `ints` żyją do końca wywołania; liczba i rodzaje parametrów zgodne z vk.xml.
        unsafe { call_thunk(func(name), ints, &[]) }.unwrap() as u32 as i32
    };

    // Rodzina kolejek i typ pamięci widoczny dla hosta.
    let mut qn = 0u32;
    unsafe { (f.get_queue_family_properties2)(pd, &mut qn, std::ptr::null_mut()) };
    let mut qf = vec![0u64; qn as usize * 5];
    for i in 0..qn as usize {
        qf[i * 5] = 1_000_059_005;
    }
    unsafe { (f.get_queue_family_properties2)(pd, &mut qn, qf.as_mut_ptr() as *mut c_void) };
    let qb = unsafe { std::slice::from_raw_parts(qf.as_ptr() as *const u8, qn as usize * 40) };
    let family = (0..qn as usize).find(|i| u32_at(qb, i * 40 + 16) & 0x7 != 0).unwrap_or_else(|| die("brak rodziny kolejek")) as u64;
    let mut mem = vec![0u64; 67];
    mem[0] = 1_000_059_006;
    unsafe { (f.get_memory_properties2)(pd, mem.as_mut_ptr() as *mut c_void) };
    let mb = unsafe { std::slice::from_raw_parts(mem.as_ptr() as *const u8, 536) };
    let types: Vec<u32> = (0..u32_at(mb, 16) as usize).map(|i| u32_at(mb, 20 + i * 8)).collect();

    let prio = [1.0f32];
    let mut qci = S::new("VkDeviceQueueCreateInfo");
    qci.u32("queueFamilyIndex", family as u32).u32("queueCount", 1).u64("pQueuePriorities", prio.as_ptr() as u64);
    let mut dci = S::new("VkDeviceCreateInfo");
    dci.u32("queueCreateInfoCount", 1).u64("pQueueCreateInfos", qci.p());
    let mut dev: u64 = 0;
    let r = call("vkCreateDevice", &[pd as u64, dci.p(), 0, &mut dev as *mut u64 as u64]);
    if r != 0 {
        die(&format!("vkCreateDevice = {r}"));
    }
    let mut queue: u64 = 0;
    call("vkGetDeviceQueue", &[dev, family, 0, &mut queue as *mut u64 as u64]);
    let mut bci = S::new("VkBufferCreateInfo");
    bci.u64("size", SIZE).u32("usage", 0x2);
    let mut buf: u64 = 0;
    let r = call("vkCreateBuffer", &[dev, bci.p(), 0, &mut buf as *mut u64 as u64]);
    if r != 0 {
        die(&format!("vkCreateBuffer = {r}"));
    }
    let mut req = S::new("VkMemoryRequirements");
    req.u32("memoryTypeBits", 0xDEAD_BEEF);
    call("vkGetBufferMemoryRequirements", &[dev, buf, req.p()]);
    let (rsize, bits) = (u64::from_le_bytes(req.bytes()[0..8].try_into().unwrap()), u32_at(req.bytes(), 16));
    if bits == 0xDEAD_BEEF || bits == 0 {
        die(&format!("zapytanie o wymagania nie zwróciło wyniku (bits={bits:#x})"));
    }
    let mt = (0..types.len()).find(|&i| bits & (1 << i) != 0 && types[i] & 0x6 == 0x6).unwrap_or_else(|| die("brak pamięci widocznej dla hosta")) as u32;
    let mut ai = S::new("VkMemoryAllocateInfo");
    ai.u64("allocationSize", rsize).u32("memoryTypeIndex", mt);
    let mut memory: u64 = 0;
    let r = call("vkAllocateMemory", &[dev, ai.p(), 0, &mut memory as *mut u64 as u64]);
    if r != 0 {
        die(&format!("vkAllocateMemory = {r}"));
    }
    call("vkBindBufferMemory", &[dev, buf, memory, 0]);
    let mut pci = S::new("VkCommandPoolCreateInfo");
    pci.u32("queueFamilyIndex", family as u32);
    let mut pool: u64 = 0;
    call("vkCreateCommandPool", &[dev, pci.p(), 0, &mut pool as *mut u64 as u64]);
    let mut cai = S::new("VkCommandBufferAllocateInfo");
    cai.u64("commandPool", pool).u32("commandBufferCount", 1);
    let mut cb: u64 = 0;
    let r = call("vkAllocateCommandBuffers", &[dev, cai.p(), &mut cb as *mut u64 as u64]);
    if r != 0 || cb == 0 {
        die(&format!("vkAllocateCommandBuffers = {r}"));
    }
    let fci = S::new("VkFenceCreateInfo");
    let mut fence: u64 = 0;
    call("vkCreateFence", &[dev, fci.p(), 0, &mut fence as *mut u64 as u64]);
    println!("przygotowanie: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);

    let mut ptr: *mut u8 = std::ptr::null_mut();
    let r = call("vkMapMemory", &[dev, memory, 0, u64::MAX, 0, &mut ptr as *mut *mut u8 as u64]);
    if r != 0 || ptr.is_null() {
        die(&format!("vkMapMemory = {r}"));
    }
    let mut cbi = S::new("VkCommandBufferBeginInfo");
    cbi.u32("flags", 1);
    let mut si = S::new("VkSubmitInfo");
    si.u32("commandBufferCount", 1).u64("pCommandBuffers", &cb as *const u64 as u64);
    let mut times = Vec::new();
    for rep in 0..reps {
        let pattern = 0xCAFE_0000u32 + rep as u32;
        let t = Instant::now();
        // SAFETY: odwzorowanie ma `rsize` bajtów (>= SIZE).
        unsafe { std::ptr::write_bytes(ptr, 0, SIZE as usize) };
        call("vkBeginCommandBuffer", &[cb, cbi.p()]);
        call("vkCmdFillBuffer", &[cb, buf, 0, u64::MAX, pattern as u64]);
        call("vkEndCommandBuffer", &[cb]);
        let r = call("vkQueueSubmit", &[queue, 1, si.p(), fence]);
        if r != 0 {
            die(&format!("vkQueueSubmit = {r}"));
        }
        let r = call("vkWaitForFences", &[dev, 1, &fence as *const u64 as u64, 1, u64::MAX]);
        if r != 0 {
            die(&format!("vkWaitForFences = {r}"));
        }
        call("vkResetFences", &[dev, 1, &fence as *const u64 as u64]);
        call("vkResetCommandBuffer", &[cb, 0]);
        let data = unsafe { std::slice::from_raw_parts(ptr, SIZE as usize) };
        if !data.chunks(4).all(|w| u32::from_le_bytes(w.try_into().unwrap()) == pattern) {
            die(&format!("powtórzenie {rep}: bufor nie zawiera wzorca {pattern:#x} (pierwsze słowo {:#x})", u32_at(data, 0)));
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("OK: GPU wypełniło bufor przez ICD ({reps} powtórzeń); czas obiegu submit+wait+odczyt: mediana {:.2} ms, min {:.2} ms", times[times.len() / 2], times[0]);
    call("vkDeviceWaitIdle", &[dev]);
    call("vkDestroyFence", &[dev, fence, 0]);
    call("vkDestroyBuffer", &[dev, buf, 0]);
    call("vkDestroyDevice", &[dev, 0]);
    unsafe { (f.destroy_instance)(inst, std::ptr::null()) };
}
