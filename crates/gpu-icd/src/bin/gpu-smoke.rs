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
    /// Pole zagnieżdżonej struktury: `member` + przesunięcie wewnątrz niej.
    fn u32o(&mut self, m: &str, extra: usize, v: u32) -> &mut S {
        let o = self.off(m) + extra;
        self.bytes()[o..o + 4].copy_from_slice(&v.to_le_bytes());
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

/// Tryb `enum`: wyliczanie urządzeń tak jak robi `vkcube` (najpierw NULL, potem bufor), z rozszerzeniami z `GPU_SMOKE_EXTS`.
fn enum_mode() {
    let lib = LibVulkan::load().unwrap_or_else(|e| die(&format!("libvulkan: {e:?}")));
    let g = GlobalFns::load(&lib).unwrap_or_else(|e| die(&format!("funkcje globalne: {e:?}")));
    let exts = std::env::var("GPU_SMOKE_EXTS").unwrap_or_default();
    let names: Vec<std::ffi::CString> = exts.split(',').filter(|e| !e.is_empty()).map(|e| std::ffi::CString::new(e).unwrap()).collect();
    let ptrs: Vec<*const std::ffi::c_char> = names.iter().map(|c| c.as_ptr()).collect();
    let mut info = [0u64; 8];
    info[0] = 1;
    info[6] = ptrs.len() as u64;
    info[7] = ptrs.as_ptr() as u64;
    let mut inst: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (g.create_instance)(info.as_ptr() as *const c_void, std::ptr::null(), &mut inst) };
    println!("vkCreateInstance({exts:?}) = {r}");
    let f = unsafe { InstanceFns::load(&lib, inst) }.unwrap_or_else(|e| die(&format!("funkcje instancji: {e:?}")));
    let mut n = 0u32;
    let r1 = unsafe { (f.enumerate_physical_devices)(inst, &mut n, std::ptr::null_mut()) };
    println!("1. wywołanie (NULL): wynik {r1}, liczba {n}");
    let mut pds = vec![0x21usize as *mut c_void; n as usize + 1];
    let r2 = unsafe { (f.enumerate_physical_devices)(inst, &mut n, pds.as_mut_ptr()) };
    println!("2. wywołanie (bufor): wynik {r2}, liczba {n}, uchwyty {:x?}", &pds[..n as usize + 1]);
    for (i, p) in pds.iter().take(n as usize).enumerate() {
        let mut props = vec![0u8; 832];
        unsafe { (f.get_properties2)(*p, props.as_mut_ptr() as *mut c_void) }; // properties2 oczekuje sType
        let mut p1 = vec![0u8; 832];
        println!("   urządzenie {i}: uchwyt {p:?}, wywołuję vkGetPhysicalDeviceProperties...");
        let fnp = unsafe { lib.proc_addr(inst, c"vkGetPhysicalDeviceProperties") }.unwrap();
        unsafe { call_thunk(fnp, &[*p as u64, p1.as_mut_ptr() as u64], &[]) };
        println!("   nazwa: {}", CStr::from_bytes_until_nul(&p1[20..276]).unwrap().to_string_lossy());
    }
    unsafe { (f.destroy_instance)(inst, std::ptr::null()) };
}

/// Tryb `swapchain`: pełna ścieżka okna na powierzchni headless: swapchain, acquire, czyszczenie obrazu, present.
fn swapchain_mode() {
    let lib = LibVulkan::load().unwrap_or_else(|e| die(&format!("libvulkan: {e:?}")));
    let g = GlobalFns::load(&lib).unwrap_or_else(|e| die(&format!("funkcje globalne: {e:?}")));
    let names = [std::ffi::CString::new("VK_KHR_surface").unwrap(), std::ffi::CString::new("VK_EXT_headless_surface").unwrap()];
    let ptrs: Vec<*const std::ffi::c_char> = names.iter().map(|c| c.as_ptr()).collect();
    let mut info = [0u64; 8];
    info[0] = 1;
    info[6] = ptrs.len() as u64;
    info[7] = ptrs.as_ptr() as u64;
    let mut inst: *mut c_void = std::ptr::null_mut();
    if unsafe { (g.create_instance)(info.as_ptr() as *const c_void, std::ptr::null(), &mut inst) } != 0 {
        die("vkCreateInstance");
    }
    let f = unsafe { InstanceFns::load(&lib, inst) }.unwrap_or_else(|e| die(&format!("funkcje instancji: {e:?}")));
    let func = |name: &str| -> *const c_void {
        let c = std::ffi::CString::new(name).unwrap();
        unsafe { lib.proc_addr(inst, &c) }.unwrap_or_else(|| die(&format!("loader nie zna {name}")))
    };
    let call = |name: &str, ints: &[u64]| -> i32 { unsafe { call_thunk(func(name), ints, &[]) }.unwrap() as u32 as i32 };
    let mut n = 8u32;
    let mut pds = vec![std::ptr::null_mut(); 8];
    unsafe { (f.enumerate_physical_devices)(inst, &mut n, pds.as_mut_ptr()) };
    let pd = pds[0] as u64;
    let mut hinfo = [0u64; 3];
    hinfo[0] = 1_000_256_000;
    let mut surface: u64 = 0;
    if call("vkCreateHeadlessSurfaceEXT", &[inst as u64, hinfo.as_ptr() as u64, 0, &mut surface as *mut u64 as u64]) != 0 {
        die("vkCreateHeadlessSurfaceEXT");
    }
    let prio = [1.0f32];
    let mut qci = S::new("VkDeviceQueueCreateInfo");
    qci.u32("queueCount", 1).u64("pQueuePriorities", prio.as_ptr() as u64);
    let ext = std::ffi::CString::new("VK_KHR_swapchain").unwrap();
    let exts = [ext.as_ptr()];
    let mut dci = S::new("VkDeviceCreateInfo");
    dci.u32("queueCreateInfoCount", 1).u64("pQueueCreateInfos", qci.p()).u32("enabledExtensionCount", 1).u64("ppEnabledExtensionNames", exts.as_ptr() as u64);
    let mut dev: u64 = 0;
    let r = call("vkCreateDevice", &[pd, dci.p(), 0, &mut dev as *mut u64 as u64]);
    if r != 0 {
        die(&format!("vkCreateDevice(VK_KHR_swapchain) = {r}"));
    }
    let mut queue: u64 = 0;
    call("vkGetDeviceQueue", &[dev, 0, 0, &mut queue as *mut u64 as u64]);
    let (w, h) = (64u32, 48u32);
    let mut sci = S::new("VkSwapchainCreateInfoKHR");
    sci.u64("surface", surface).u32("minImageCount", 3).u32("imageFormat", 44).u32o("imageExtent", 0, w).u32o("imageExtent", 4, h);
    sci.u32("imageArrayLayers", 1).u32("imageUsage", 0x12).u32("preTransform", 1).u32("compositeAlpha", 1).u32("presentMode", 2).u32("clipped", 1);
    let mut sc: u64 = 0;
    let r = call("vkCreateSwapchainKHR", &[dev, sci.p(), 0, &mut sc as *mut u64 as u64]);
    if r != 0 || sc == 0 {
        die(&format!("vkCreateSwapchainKHR = {r}"));
    }
    let mut cnt = 0u32;
    call("vkGetSwapchainImagesKHR", &[dev, sc, &mut cnt as *mut u32 as u64, 0]);
    let mut images = vec![0u64; cnt as usize];
    call("vkGetSwapchainImagesKHR", &[dev, sc, &mut cnt as *mut u32 as u64, images.as_mut_ptr() as u64]);
    println!("swapchain: {cnt} obrazy");
    let sem_ci = S::new("VkSemaphoreCreateInfo");
    let (mut sem_acq, mut sem_done) = (0u64, 0u64);
    call("vkCreateSemaphore", &[dev, sem_ci.p(), 0, &mut sem_acq as *mut u64 as u64]);
    call("vkCreateSemaphore", &[dev, sem_ci.p(), 0, &mut sem_done as *mut u64 as u64]);
    let mut pci = S::new("VkCommandPoolCreateInfo");
    pci.u32("flags", 2);
    let mut pool: u64 = 0;
    call("vkCreateCommandPool", &[dev, pci.p(), 0, &mut pool as *mut u64 as u64]);
    let mut cai = S::new("VkCommandBufferAllocateInfo");
    cai.u64("commandPool", pool).u32("commandBufferCount", 1);
    let mut cb: u64 = 0;
    call("vkAllocateCommandBuffers", &[dev, cai.p(), &mut cb as *mut u64 as u64]);
    let color = [0.2f32, 0.4, 0.8, 1.0];
    let mut range = S::new("VkImageSubresourceRange");
    range.u32("aspectMask", 1).u32("levelCount", 1).u32("layerCount", 1);
    let begin = S::new("VkCommandBufferBeginInfo");
    for frame in 0..3 {
        let mut idx = 99u32;
        let r = call("vkAcquireNextImageKHR", &[dev, sc, u64::MAX, sem_acq, 0, &mut idx as *mut u32 as u64]);
        if r != 0 || idx as usize >= images.len() {
            die(&format!("vkAcquireNextImageKHR = {r}, indeks {idx}"));
        }
        let image = images[idx as usize];
        let barrier = |old: u32, new: u32, src: u32, dst: u32| {
            let mut b = S::new("VkImageMemoryBarrier");
            b.u32("srcAccessMask", src).u32("dstAccessMask", dst).u32("oldLayout", old).u32("newLayout", new);
            b.u32("srcQueueFamilyIndex", u32::MAX).u32("dstQueueFamilyIndex", u32::MAX).u64("image", image);
            b.u32o("subresourceRange", 0, 1).u32o("subresourceRange", 8, 1).u32o("subresourceRange", 16, 1);
            b
        };
        let (b1, b2) = (barrier(0, 7, 0, 0x1000), barrier(7, 1_000_001_002, 0x1000, 0));
        call("vkResetCommandBuffer", &[cb, 0]);
        call("vkBeginCommandBuffer", &[cb, begin.p()]);
        call("vkCmdPipelineBarrier", &[cb, 0x1, 0x1000, 0, 0, 0, 0, 0, 1, b1.p()]);
        call("vkCmdClearColorImage", &[cb, image, 7, color.as_ptr() as u64, 1, range.p()]);
        call("vkCmdPipelineBarrier", &[cb, 0x1000, 0x2000, 0, 0, 0, 0, 0, 1, b2.p()]);
        call("vkEndCommandBuffer", &[cb]);
        let mask = [0x1000u32];
        let mut si = S::new("VkSubmitInfo");
        si.u32("waitSemaphoreCount", 1).u64("pWaitSemaphores", &sem_acq as *const u64 as u64).u64("pWaitDstStageMask", mask.as_ptr() as u64);
        si.u32("commandBufferCount", 1).u64("pCommandBuffers", &cb as *const u64 as u64).u32("signalSemaphoreCount", 1).u64("pSignalSemaphores", &sem_done as *const u64 as u64);
        let r = call("vkQueueSubmit", &[queue, 1, si.p(), 0]);
        if r != 0 {
            die(&format!("vkQueueSubmit = {r}"));
        }
        let mut pi = S::new("VkPresentInfoKHR");
        pi.u32("waitSemaphoreCount", 1).u64("pWaitSemaphores", &sem_done as *const u64 as u64).u32("swapchainCount", 1);
        pi.u64("pSwapchains", &sc as *const u64 as u64).u64("pImageIndices", &idx as *const u32 as u64);
        let r = call("vkQueuePresentKHR", &[queue, pi.p()]);
        if r != 0 && r != 1_000_001_003 {
            die(&format!("vkQueuePresentKHR (klatka {frame}) = {r}"));
        }
    }
    call("vkDeviceWaitIdle", &[dev]);
    println!("OK: 3 klatki narysowane na zdalnej karcie przez swapchain i wyświetlone lokalnie");
    call("vkDestroySwapchainKHR", &[dev, sc, 0]);
    call("vkDestroyDevice", &[dev, 0]);
    call("vkDestroySurfaceKHR", &[inst as u64, surface, 0]);
    unsafe { (f.destroy_instance)(inst, std::ptr::null()) };
}

/// Tryb `surface`: powierzchnia headless przez loader i nasz ICD; zapytania o możliwości okna.
fn surface_mode() {
    let lib = LibVulkan::load().unwrap_or_else(|e| die(&format!("libvulkan: {e:?}")));
    let g = GlobalFns::load(&lib).unwrap_or_else(|e| die(&format!("funkcje globalne: {e:?}")));
    let names = [std::ffi::CString::new("VK_KHR_surface").unwrap(), std::ffi::CString::new("VK_EXT_headless_surface").unwrap()];
    let ptrs: Vec<*const std::ffi::c_char> = names.iter().map(|c| c.as_ptr()).collect();
    let mut info = [0u64; 8];
    info[0] = 1;
    info[6] = ptrs.len() as u64;
    info[7] = ptrs.as_ptr() as u64;
    let mut inst: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (g.create_instance)(info.as_ptr() as *const c_void, std::ptr::null(), &mut inst) };
    if r != 0 {
        die(&format!("vkCreateInstance z powierzchnią = {r} (ICD nie ogłasza rozszerzeń powierzchni?)"));
    }
    let f = unsafe { InstanceFns::load(&lib, inst) }.unwrap_or_else(|e| die(&format!("funkcje instancji: {e:?}")));
    let func = |name: &str| -> *const c_void {
        let c = std::ffi::CString::new(name).unwrap();
        unsafe { lib.proc_addr(inst, &c) }.unwrap_or_else(|| die(&format!("loader nie zna {name}")))
    };
    let call = |name: &str, ints: &[u64]| -> i32 { unsafe { call_thunk(func(name), ints, &[]) }.unwrap() as u32 as i32 };
    let mut n = 8u32;
    let mut pds = vec![std::ptr::null_mut(); 8];
    unsafe { (f.enumerate_physical_devices)(inst, &mut n, pds.as_mut_ptr()) };
    if n == 0 {
        die("brak urządzeń");
    }
    let pd = pds[0] as u64;
    let mut hinfo = [0u64; 3];
    hinfo[0] = 1_000_256_000; // VK_STRUCTURE_TYPE_HEADLESS_SURFACE_CREATE_INFO_EXT
    let mut surface: u64 = 0;
    let r = call("vkCreateHeadlessSurfaceEXT", &[inst as u64, hinfo.as_ptr() as u64, 0, &mut surface as *mut u64 as u64]);
    if r != 0 || surface == 0 {
        die(&format!("vkCreateHeadlessSurfaceEXT = {r}"));
    }
    let mut sup = 99u32;
    let r = call("vkGetPhysicalDeviceSurfaceSupportKHR", &[pd, 0, surface, &mut sup as *mut u32 as u64]);
    println!("wsparcie prezentacji (rodzina 0): kod {r}, wartość {sup}");
    let mut caps = vec![0u8; 56];
    let r = call("vkGetPhysicalDeviceSurfaceCapabilitiesKHR", &[pd, surface, caps.as_mut_ptr() as u64]);
    if r != 0 {
        die(&format!("capabilities = {r}"));
    }
    let (minc, maxc, usage) = (u32_at(&caps, 0), u32_at(&caps, 4), u32_at(&caps, 48));
    println!("możliwości: minImageCount {minc}, maxImageCount {maxc}, usage {usage:#x}, currentExtent {}x{}", u32_at(&caps, 8), u32_at(&caps, 12));
    let mut fnum = 0u32;
    call("vkGetPhysicalDeviceSurfaceFormatsKHR", &[pd, surface, &mut fnum as *mut u32 as u64, 0]);
    let mut fbuf = vec![0u8; fnum as usize * 8];
    call("vkGetPhysicalDeviceSurfaceFormatsKHR", &[pd, surface, &mut fnum as *mut u32 as u64, fbuf.as_mut_ptr() as u64]);
    let formats: Vec<u32> = (0..fnum as usize).map(|i| u32_at(&fbuf, i * 8)).collect();
    let mut mnum = 0u32;
    call("vkGetPhysicalDeviceSurfacePresentModesKHR", &[pd, surface, &mut mnum as *mut u32 as u64, 0]);
    let mut mbuf = vec![0u8; mnum as usize * 4];
    call("vkGetPhysicalDeviceSurfacePresentModesKHR", &[pd, surface, &mut mnum as *mut u32 as u64, mbuf.as_mut_ptr() as u64]);
    let modes: Vec<u32> = (0..mnum as usize).map(|i| u32_at(&mbuf, i * 4)).collect();
    println!("formaty: {formats:?}, tryby prezentacji: {modes:?}");
    if (minc, maxc) != (3, 3) || usage & 0x10 == 0 || !modes.contains(&2) || modes.contains(&1) || formats.iter().any(|f| ![37, 43, 44, 50].contains(f)) {
        die("odpowiedzi o powierzchni niezgodne z oczekiwaniami (3 obrazy, COLOR_ATTACHMENT, FIFO bez MAILBOX, formaty 8-bitowe)");
    }
    println!("OK: ICD odpowiada o powierzchni okna (3 obrazy, FIFO, formaty {} szt.)", formats.len());
    call("vkDestroySurfaceKHR", &[inst as u64, surface, 0]);
    unsafe { (f.destroy_instance)(inst, std::ptr::null()) };
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("swapchain") {
        swapchain_mode();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("enum") {
        enum_mode();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("surface") {
        surface_mode();
        return;
    }
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

    // ---------- obraz: clear + copy do bufora (bariery z 10 parametrami, unia VkClearColorValue) ----------
    {
        let (w, h) = (64u32, 64u32);
        let mut ici = S::new("VkImageCreateInfo");
        ici.u32("imageType", 1).u32("format", 37).u32o("extent", 0, w).u32o("extent", 4, h).u32o("extent", 8, 1);
        ici.u32("mipLevels", 1).u32("arrayLayers", 1).u32("samples", 1).u32("usage", 0x3);
        let mut image: u64 = 0;
        let r = call("vkCreateImage", &[dev, ici.p(), 0, &mut image as *mut u64 as u64]);
        if r != 0 {
            die(&format!("vkCreateImage = {r}"));
        }
        let mut ireq = S::new("VkMemoryRequirements");
        call("vkGetImageMemoryRequirements", &[dev, image, ireq.p()]);
        let (isize_, ibits) = (u64::from_le_bytes(ireq.bytes()[0..8].try_into().unwrap()), u32_at(ireq.bytes(), 16));
        let imt = (0..types.len()).find(|&i| ibits & (1 << i) != 0 && types[i] & 0x1 != 0).or_else(|| (0..types.len()).find(|&i| ibits & (1 << i) != 0)).unwrap_or_else(|| die("brak typu pamięci dla obrazu")) as u32;
        let mut iai = S::new("VkMemoryAllocateInfo");
        iai.u64("allocationSize", isize_).u32("memoryTypeIndex", imt);
        let mut imem: u64 = 0;
        let r = call("vkAllocateMemory", &[dev, iai.p(), 0, &mut imem as *mut u64 as u64]);
        if r != 0 {
            die(&format!("vkAllocateMemory(obraz) = {r}"));
        }
        call("vkBindImageMemory", &[dev, image, imem, 0]);
        // bufor odczytu
        let out_size = (w * h * 4) as u64;
        let mut obci = S::new("VkBufferCreateInfo");
        obci.u64("size", out_size).u32("usage", 0x2);
        let mut obuf: u64 = 0;
        call("vkCreateBuffer", &[dev, obci.p(), 0, &mut obuf as *mut u64 as u64]);
        let mut oreq = S::new("VkMemoryRequirements");
        call("vkGetBufferMemoryRequirements", &[dev, obuf, oreq.p()]);
        let (osz, obits) = (u64::from_le_bytes(oreq.bytes()[0..8].try_into().unwrap()), u32_at(oreq.bytes(), 16));
        let omt = (0..types.len()).find(|&i| obits & (1 << i) != 0 && types[i] & 0x6 == 0x6).unwrap_or_else(|| die("brak pamięci hosta dla odczytu")) as u32;
        let mut oai = S::new("VkMemoryAllocateInfo");
        oai.u64("allocationSize", osz).u32("memoryTypeIndex", omt);
        let mut omem: u64 = 0;
        call("vkAllocateMemory", &[dev, oai.p(), 0, &mut omem as *mut u64 as u64]);
        call("vkBindBufferMemory", &[dev, obuf, omem, 0]);
        let mut optr: *mut u8 = std::ptr::null_mut();
        call("vkMapMemory", &[dev, omem, 0, u64::MAX, 0, &mut optr as *mut *mut u8 as u64]);

        let range = |s: &mut S, name: &str| {
            s.u32o(name, 0, 1).u32o(name, 8, 1).u32o(name, 16, 1); // aspectMask, levelCount, layerCount
        };
        let barrier = |old: u32, new: u32, src: u32, dst: u32| {
            let mut b = S::new("VkImageMemoryBarrier");
            b.u32("srcAccessMask", src).u32("dstAccessMask", dst).u32("oldLayout", old).u32("newLayout", new);
            b.u32("srcQueueFamilyIndex", u32::MAX).u32("dstQueueFamilyIndex", u32::MAX).u64("image", image);
            range(&mut b, "subresourceRange");
            b
        };
        let to_dst = barrier(0, 7, 0, 0x1000); // UNDEFINED -> TRANSFER_DST_OPTIMAL
        let to_src = barrier(7, 6, 0x1000, 0x800); // -> TRANSFER_SRC_OPTIMAL
        let color = [0.25f32, 0.5, 0.75, 1.0];
        let mut sub = S::new("VkImageSubresourceRange");
        range(&mut sub, "aspectMask"); // aspectMask @0, levelCount @8, layerCount @16 względem początku struktury
        let mut copy = S::new("VkBufferImageCopy");
        copy.u32o("imageSubresource", 0, 1).u32o("imageSubresource", 12, 1).u32o("imageExtent", 0, w).u32o("imageExtent", 4, h).u32o("imageExtent", 8, 1);
        call("vkResetCommandBuffer", &[cb, 0]);
        call("vkBeginCommandBuffer", &[cb, cbi.p()]);
        // vkCmdPipelineBarrier(cb, srcStage, dstStage, depFlags, memCount, pMem, bufCount, pBuf, imgCount, pImg): 10 parametrów
        call("vkCmdPipelineBarrier", &[cb, 0x1, 0x1000, 0, 0, 0, 0, 0, 1, to_dst.p()]);
        call("vkCmdClearColorImage", &[cb, image, 7, color.as_ptr() as u64, 1, sub.p()]);
        call("vkCmdPipelineBarrier", &[cb, 0x1000, 0x1000, 0, 0, 0, 0, 0, 1, to_src.p()]);
        call("vkCmdCopyImageToBuffer", &[cb, image, 6, obuf, 1, copy.p()]);
        call("vkEndCommandBuffer", &[cb]);
        let r = call("vkQueueSubmit", &[queue, 1, si.p(), fence]);
        if r != 0 {
            die(&format!("vkQueueSubmit(obraz) = {r}"));
        }
        let r = call("vkWaitForFences", &[dev, 1, &fence as *const u64 as u64, 1, u64::MAX]);
        if r != 0 {
            die(&format!("vkWaitForFences(obraz) = {r}"));
        }
        let px = unsafe { std::slice::from_raw_parts(optr, out_size as usize) };
        let want = [64u8, 128, 191, 255];
        let bad = px.chunks(4).filter(|c| c.iter().zip(want).any(|(a, b)| (*a as i32 - b as i32).abs() > 1)).count();
        if bad != 0 {
            die(&format!("obraz: {bad}/{} pikseli różni się od oczekiwanego {want:?}; pierwszy piksel {:?}", w * h, &px[..4]));
        }
        println!("OK: obraz {w}x{h} wyczyszczony na zdalnej karcie i odczytany (piksel {:?})", &px[..4]);
    }
    call("vkDeviceWaitIdle", &[dev]);
    call("vkDestroyFence", &[dev, fence, 0]);
    call("vkDestroyBuffer", &[dev, buf, 0]);
    call("vkDestroyDevice", &[dev, 0]);
    unsafe { (f.destroy_instance)(inst, std::ptr::null()) };
}
