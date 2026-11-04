//! Wirtualny swapchain: obrazy żyją na serwerze, a klatka wraca przez odczyt bufora i jest wyświetlana lokalnie.
//!
//! Naiwna wersja bazowa (M2b): `Present` wysyła polecenie kopiowania obrazu do bufora odczytu, czeka na fence i czyta
//! bufor (`MemRead`), a lokalny prezenter (Intel) wyświetla klatkę. `Acquire` nie czeka na sieć: indeks z puli, a semafor
//! i fence aplikacji są sygnalizowane po stronie serwera pustym `vkQueueSubmit` (aplikacja czeka na semafory serwera).

use std::ffi::c_void;

use gpu_proto::meta::find_struct;

use crate::fwd::{exec_named, exec_named_async};
use crate::video::VideoThread;
use crate::present::{rgba_to_bgra, Presenter};
use crate::sbuild::S;
use crate::{with_local, InstState, InstanceObj, CURRENT};

const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_OUT_OF_DATE: i32 = -1_000_001_004;
const IMAGES: usize = 3;
const LAYOUT_PRESENT_SRC: u32 = 1_000_001_002;
const LAYOUT_TRANSFER_SRC: u32 = 6;
const WHOLE: u64 = u64::MAX;

pub struct Swapchain {
    device: u64,
    queue: u64,
    width: u32,
    height: u32,
    format: u32,
    images: Vec<u64>,
    cbs: Vec<u64>,
    fence: u64,
    readback_mem: u64,
    /// Lokalny prezenter tworzymy leniwie przy pierwszym `Present` (poza callbackami okna aplikacji).
    presenter: Option<Presenter>,
    surface: u64,
    next: usize,
    frames: u64,
    /// Połączenie wideo (asynchroniczny `Present`); `None` = tryb synchroniczny (odczyt w wątku aplikacji).
    video: Option<VideoThread>,
}

fn state() -> Option<&'static InstanceObj> {
    // SAFETY: wskaźnik ustawia `create_instance` i zeruje `destroy_instance`.
    unsafe { CURRENT.load(std::sync::atomic::Ordering::SeqCst).as_ref() }
}

/// Wartość członka struktury aplikacji (offset z metadanych).
unsafe fn rd<T: Copy>(struct_name: &str, ptr: *const u8, member: &str) -> Option<T> {
    let m = find_struct(struct_name)?.members().iter().find(|m| m.name == member)?;
    Some(std::ptr::read_unaligned(ptr.add(m.offset as usize) as *const T))
}

/// Kodek ramek wybrany ustawieniem `GPU_ICD_CODEC` (`auto`, `raw`, `lz4`, `h264`, `hevc`) spośród tych, które ICD umie dekodować
/// i serwer koduje (LZ4, surowy i H.264 przez NVENC/VA-API; HEVC nie, bo i965 go nie dekoduje). Niedostępny wybór => ostrzeżenie i `auto`.
fn pick_codec() -> u8 {
    use gpu_proto::video::{choose_codec, Codec};
    // Kolejność = preferencja dla `auto`; H.264 tylko na wyraźne żądanie, dopóki nie zmierzymy, że jest lepszy.
    const AVAILABLE: &[Codec] = &[Codec::Lz4, Codec::Raw, Codec::H264];
    let setting = std::env::var("GPU_ICD_CODEC").unwrap_or_default();
    let chosen = choose_codec(&setting, AVAILABLE).unwrap_or_else(|| {
        eprintln!("gpu-icd: kodek {setting:?} niedostępny (dostępne: lz4, raw, h264); używam lz4");
        Codec::Lz4
    });
    chosen as u8
}

/// Wybiera typ pamięci zdalnej karty: dozwolony przez `bits` i mający wszystkie `want` bity właściwości.
fn pick_memory_type(props: &[u8], bits: u32, want: u32) -> Option<u32> {
    let count = u32::from_le_bytes(props.get(16..20)?.try_into().ok()?) as usize;
    (0..count.min(32)).find(|&i| {
        let f = props.get(20 + i * 8..24 + i * 8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
        bits & (1 << i) != 0 && f & want == want
    }).map(|i| i as u32)
}

unsafe fn remote_pdev_props(st: &mut InstState, dev: u64) -> Option<Vec<u8>> {
    let dev_id = std::ptr::read_unaligned((dev as *const u8).add(8) as *const u64);
    let pd = *st.device_pdev.get(&dev_id)?;
    st.query_props(pd)
}

unsafe fn alloc_bound(st: &mut InstState, dev: u64, props: &[u8], kind: &str, obj: u64, want: u32) -> Result<u64, i32> {
    let (req_fn, bind_fn) = if kind == "image" { ("vkGetImageMemoryRequirements", "vkBindImageMemory") } else { ("vkGetBufferMemoryRequirements", "vkBindBufferMemory") };
    let mut req = S::new("VkMemoryRequirements");
    req.u32("memoryTypeBits", 0xDEAD_BEEF);
    if exec_named(st, req_fn, &mut [dev, obj, req.p()]) < 0 {
        return Err(VK_ERROR_INITIALIZATION_FAILED);
    }
    let rb = req.bytes();
    let (size, bits) = (u64::from_le_bytes(rb[0..8].try_into().unwrap_or([0; 8])), u32::from_le_bytes(rb[16..20].try_into().unwrap_or([0; 4])));
    // Preferujemy pełne `want`, potem bez bitu CACHED (0x8), na końcu cokolwiek dozwolonego.
    let mt = pick_memory_type(props, bits, want).or_else(|| pick_memory_type(props, bits, want & !0x8)).or_else(|| pick_memory_type(props, bits, 0)).ok_or(VK_ERROR_INITIALIZATION_FAILED)?;
    let mut ai = S::new("VkMemoryAllocateInfo");
    ai.u64("allocationSize", size).u32("memoryTypeIndex", mt);
    let mut mem = 0u64;
    if exec_named(st, "vkAllocateMemory", &mut [dev, ai.p(), 0, &mut mem as *mut u64 as u64]) < 0 {
        return Err(VK_ERROR_INITIALIZATION_FAILED);
    }
    if exec_named(st, bind_fn, &mut [dev, obj, mem, 0]) < 0 {
        return Err(VK_ERROR_INITIALIZATION_FAILED);
    }
    Ok(mem)
}

unsafe fn create_inner(st: &mut InstState, dev: u64, info: *const u8) -> Result<u64, i32> {
    let surface: u64 = rd("VkSwapchainCreateInfoKHR", info, "surface").ok_or(VK_ERROR_INITIALIZATION_FAILED)?;
    let format: u32 = rd("VkSwapchainCreateInfoKHR", info, "imageFormat").ok_or(VK_ERROR_INITIALIZATION_FAILED)?;
    let usage: u32 = rd("VkSwapchainCreateInfoKHR", info, "imageUsage").ok_or(VK_ERROR_INITIALIZATION_FAILED)?;
    let ext_off = find_struct("VkSwapchainCreateInfoKHR").and_then(|m| m.members().iter().find(|m| m.name == "imageExtent")).map(|m| m.offset as usize).ok_or(VK_ERROR_INITIALIZATION_FAILED)?;
    let (w, h) = (std::ptr::read_unaligned(info.add(ext_off) as *const u32), std::ptr::read_unaligned(info.add(ext_off + 4) as *const u32));
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err(VK_ERROR_INITIALIZATION_FAILED);
    }
    let props = remote_pdev_props(st, dev).ok_or(VK_ERROR_INITIALIZATION_FAILED)?;

    let mut images = Vec::new();
    for _ in 0..IMAGES {
        let mut ici = S::new("VkImageCreateInfo");
        ici.u32("imageType", 1).u32("format", format).u32o("extent", 0, w).u32o("extent", 4, h).u32o("extent", 8, 1);
        ici.u32("mipLevels", 1).u32("arrayLayers", 1).u32("samples", 1).u32("usage", usage | 0x1);
        let mut id = 0u64;
        if exec_named(st, "vkCreateImage", &mut [dev, ici.p(), 0, &mut id as *mut u64 as u64]) < 0 {
            return Err(VK_ERROR_INITIALIZATION_FAILED);
        }
        alloc_bound(st, dev, &props, "image", id, 0x1)?;
        images.push(id);
    }
    // Bufor odczytu (widoczny dla hosta) na jedną klatkę.
    let size = w as u64 * h as u64 * 4;
    let mut bci = S::new("VkBufferCreateInfo");
    bci.u64("size", size).u32("usage", 0x2);
    let mut rbuf = 0u64;
    if exec_named(st, "vkCreateBuffer", &mut [dev, bci.p(), 0, &mut rbuf as *mut u64 as u64]) < 0 {
        return Err(VK_ERROR_INITIALIZATION_FAILED);
    }
    // Bufor odczytu w pamięci HOST_VISIBLE|COHERENT|CACHED: odczyt z pamięci bez cache jest wielokrotnie wolniejszy.
    let readback_mem = alloc_bound(st, dev, &props, "buffer", rbuf, 0xE)?;

    let mut queue = 0u64;
    exec_named(st, "vkGetDeviceQueue", &mut [dev, 0, 0, &mut queue as *mut u64 as u64]);
    let mut pci = S::new("VkCommandPoolCreateInfo");
    pci.u32("flags", 0x2);
    let mut pool = 0u64;
    if exec_named(st, "vkCreateCommandPool", &mut [dev, pci.p(), 0, &mut pool as *mut u64 as u64]) < 0 {
        return Err(VK_ERROR_INITIALIZATION_FAILED);
    }
    let mut cai = S::new("VkCommandBufferAllocateInfo");
    cai.u64("commandPool", pool).u32("commandBufferCount", IMAGES as u32);
    let mut cbs = vec![0u64; IMAGES];
    if exec_named(st, "vkAllocateCommandBuffers", &mut [dev, cai.p(), cbs.as_mut_ptr() as u64]) < 0 {
        return Err(VK_ERROR_INITIALIZATION_FAILED);
    }
    // Każdy bufor poleceń kopiuje swój obraz (PRESENT_SRC -> TRANSFER_SRC) do bufora odczytu i oddaje układ.
    let begin = S::new("VkCommandBufferBeginInfo");
    for (i, cb) in cbs.iter().enumerate() {
        let barrier = |old: u32, new: u32, src: u32, dst: u32| {
            let mut b = S::new("VkImageMemoryBarrier");
            b.u32("srcAccessMask", src).u32("dstAccessMask", dst).u32("oldLayout", old).u32("newLayout", new);
            b.u32("srcQueueFamilyIndex", u32::MAX).u32("dstQueueFamilyIndex", u32::MAX).u64("image", images[i]);
            b.u32o("subresourceRange", 0, 1).u32o("subresourceRange", 8, 1).u32o("subresourceRange", 16, 1);
            b
        };
        let (b1, b2) = (barrier(LAYOUT_PRESENT_SRC, LAYOUT_TRANSFER_SRC, 0, 0x800), barrier(LAYOUT_TRANSFER_SRC, LAYOUT_PRESENT_SRC, 0x800, 0));
        let mut copy = S::new("VkBufferImageCopy");
        copy.u32o("imageSubresource", 0, 1).u32o("imageSubresource", 12, 1).u32o("imageExtent", 0, w).u32o("imageExtent", 4, h).u32o("imageExtent", 8, 1);
        exec_named(st, "vkBeginCommandBuffer", &mut [*cb, begin.p()]);
        exec_named(st, "vkCmdPipelineBarrier", &mut [*cb, 0x10000, 0x1000, 0, 0, 0, 0, 0, 1, b1.p()]);
        exec_named(st, "vkCmdCopyImageToBuffer", &mut [*cb, images[i], LAYOUT_TRANSFER_SRC as u64, rbuf, 1, copy.p()]);
        exec_named(st, "vkCmdPipelineBarrier", &mut [*cb, 0x1000, 0x2000, 0, 0, 0, 0, 0, 1, b2.p()]);
        exec_named(st, "vkEndCommandBuffer", &mut [*cb]);
    }
    let fci = S::new("VkFenceCreateInfo");
    let mut fence = 0u64;
    exec_named(st, "vkCreateFence", &mut [dev, fci.p(), 0, &mut fence as *mut u64 as u64]);
    // Połączenie wideo: serwer wypycha ramki, a `Present` nie czeka na odczyt (zapas: GPU_ICD_SYNC_PRESENT=1).
    let video = if std::env::var("GPU_ICD_SYNC_PRESENT").is_ok() {
        None
    } else {
        match st.client.call(&gpu_proto::msg::Request::SessionId) {
            Ok(gpu_proto::msg::Response::Handle(session)) => VideoThread::start(&st.addr, &st.token, session, surface, w, h).map_err(|e| eprintln!("gpu-icd: wideo niedostępne, tryb synchroniczny: {e}")).ok(),
            _ => None,
        }
    };
    let id = st.alloc_id();
    st.swapchains.insert(id, Swapchain { device: dev, queue, width: w, height: h, format, images, cbs, fence, readback_mem, presenter: None, surface, next: 0, frames: 0, video });
    Ok(id)
}

pub unsafe extern "C" fn create_swapchain(dev: u64, info: *const u8, _alloc: *const c_void, out: *mut u64) -> i32 {
    let (Some(inst), false, false) = (state(), info.is_null(), out.is_null()) else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut st = crate::fwd::lock_state(inst);
    match create_inner(&mut st, dev, info) {
        Ok(id) => {
            *out = id;
            0
        }
        Err(e) => e,
    }
}

pub unsafe extern "C" fn destroy_swapchain(dev: u64, sc: u64, _alloc: *const c_void) {
    let Some(inst) = state() else { return };
    let mut st = crate::fwd::lock_state(inst);
    let Some(mut sw) = st.swapchains.remove(&sc) else { return };
    if let Some(v) = sw.video.as_mut() {
        st.flush();
        let stats = v.stop();
        if std::env::var("GPU_ICD_STATS").is_ok() {
            eprintln!("gpu-icd: wideo zatrzymane po {} klatkach", stats.frames);
        }
    }
    exec_named(&mut st, "vkDeviceWaitIdle", &mut [dev]);
    exec_named(&mut st, "vkDestroyFence", &mut [dev, sw.fence, 0]);
    for img in &sw.images {
        exec_named(&mut st, "vkDestroyImage", &mut [dev, *img, 0]);
    }
    if let Some(p) = sw.presenter.as_mut() {
        with_local(|l| p.destroy(l));
    }
}

pub unsafe extern "C" fn get_swapchain_images(_dev: u64, sc: u64, count: *mut u32, out: *mut u64) -> i32 {
    let (Some(inst), Some(count)) = (state(), count.as_mut()) else { return VK_ERROR_INITIALIZATION_FAILED };
    let st = crate::fwd::lock_state(inst);
    let Some(sw) = st.swapchains.get(&sc) else { return VK_ERROR_INITIALIZATION_FAILED };
    if out.is_null() {
        *count = sw.images.len() as u32;
        return 0;
    }
    let n = (*count as usize).min(sw.images.len());
    for (i, img) in sw.images.iter().take(n).enumerate() {
        *out.add(i) = *img;
    }
    *count = n as u32;
    if n < sw.images.len() {
        5
    } else {
        0
    }
}

unsafe fn acquire(st: &mut InstState, sc: u64, sem: u64, fence: u64, out: *mut u32) -> i32 {
    let Some(sw) = st.swapchains.get_mut(&sc) else { return VK_ERROR_OUT_OF_DATE };
    let idx = sw.next;
    sw.next = (sw.next + 1) % sw.images.len();
    let queue = sw.queue;
    if !out.is_null() {
        *out = idx as u32;
    }
    if sem != 0 || fence != 0 {
        // Aplikacja będzie czekać na ten semafor/fence w swoim submit na serwerze: sygnalizuje je pusty submit po stronie serwera.
        let mut si = S::new("VkSubmitInfo");
        si.u32("signalSemaphoreCount", (sem != 0) as u32).u64("pSignalSemaphores", &sem as *const u64 as u64);
        exec_named(st, "vkQueueSubmit", &mut [queue, 1, si.p(), fence]);
    }
    0
}

pub unsafe extern "C" fn acquire_next_image(_dev: u64, sc: u64, _timeout: u64, sem: u64, fence: u64, out: *mut u32) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    crate::watchdog::set_named("vkAcquireNextImageKHR", crate::watchdog::WAIT_LOCK);
    let _wd = crate::fwd::WdGuard;
    let mut st = crate::fwd::lock_state(inst);
    crate::watchdog::phase(crate::watchdog::HOLD_LOCK, 0);
    acquire(&mut st, sc, sem, fence, out)
}

pub unsafe extern "C" fn acquire_next_image2(_dev: u64, info: *const u8, out: *mut u32) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    let (Some(sc), Some(sem), Some(fence)) = (rd::<u64>("VkAcquireNextImageInfoKHR", info, "swapchain"), rd::<u64>("VkAcquireNextImageInfoKHR", info, "semaphore"), rd::<u64>("VkAcquireNextImageInfoKHR", info, "fence")) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    crate::watchdog::set_named("vkAcquireNextImage2KHR", crate::watchdog::WAIT_LOCK);
    let _wd = crate::fwd::WdGuard;
    let mut st = crate::fwd::lock_state(inst);
    crate::watchdog::phase(crate::watchdog::HOLD_LOCK, 0);
    acquire(&mut st, sc, sem, fence, out)
}

pub unsafe extern "C" fn queue_present(queue: u64, info: *const u8) -> i32 {
    let (Some(inst), false) = (state(), info.is_null()) else { return VK_ERROR_INITIALIZATION_FAILED };
    crate::watchdog::set_named("vkQueuePresentKHR", crate::watchdog::WAIT_LOCK);
    let _wd = crate::fwd::WdGuard;
    let mut st = crate::fwd::lock_state(inst);
    crate::watchdog::phase(crate::watchdog::HOLD_LOCK, 0);
    let (Some(nwait), Some(waits), Some(nsc), Some(scs), Some(idxs), Some(results)) = (
        rd::<u32>("VkPresentInfoKHR", info, "waitSemaphoreCount"),
        rd::<*const u64>("VkPresentInfoKHR", info, "pWaitSemaphores"),
        rd::<u32>("VkPresentInfoKHR", info, "swapchainCount"),
        rd::<*const u64>("VkPresentInfoKHR", info, "pSwapchains"),
        rd::<*const u32>("VkPresentInfoKHR", info, "pImageIndices"),
        rd::<*mut i32>("VkPresentInfoKHR", info, "pResults"),
    ) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut overall = 0;
    for k in 0..nsc as usize {
        let sc = *scs.add(k);
        let idx = *idxs.add(k) as usize;
        let r = present_one(&mut st, queue, sc, idx, if k == 0 { nwait } else { 0 }, waits);
        if !results.is_null() {
            *results.add(k) = r;
        }
        if r < 0 && overall >= 0 {
            overall = r;
        }
    }
    overall
}

unsafe fn present_one(st: &mut InstState, queue: u64, sc: u64, idx: usize, nwait: u32, waits: *const u64) -> i32 {
    let Some(sw) = st.swapchains.get_mut(&sc) else { return VK_ERROR_OUT_OF_DATE };
    let (dev, fence, readback, fmt, w, h) = (sw.device, sw.fence, sw.readback_mem, sw.format, sw.width, sw.height);
    let Some(cb) = sw.cbs.get(idx).copied() else { return VK_ERROR_OUT_OF_DATE };
    let t0 = std::time::Instant::now();
    let stages = vec![0x1000u32; nwait as usize];
    let mut si = S::new("VkSubmitInfo");
    si.u32("waitSemaphoreCount", nwait).u64("pWaitSemaphores", waits as u64).u64("pWaitDstStageMask", stages.as_ptr() as u64);
    si.u32("commandBufferCount", 1).u64("pCommandBuffers", &cb as *const u64 as u64);
    if st.swapchains.get(&sc).is_some_and(|s| s.video.is_some()) {
        // Asynchronicznie: submit kopii, oczekiwanie serwera na fence i polecenie wysłania ramki; aplikacja nie czeka.
        let seq = st.swapchains.get_mut(&sc).map(|s| { s.frames += 1; s.frames as u32 }).unwrap_or(0);
        if exec_named_async(st, "vkQueueSubmit", &mut [queue, 1, si.p(), fence]) < 0 {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        exec_named_async(st, "vkWaitForFences", &mut [dev, 1, &fence as *const u64 as u64, 1, WHOLE]);
        exec_named_async(st, "vkResetFences", &mut [dev, 1, &fence as *const u64 as u64]);
        st.flush();
        let format = if matches!(fmt, 37 | 43) { 2u8 } else { 1u8 };
        // Kodek ramek: domyślnie LZ4 (bezstratny); `GPU_ICD_CODEC=raw` wymusza surowe piksele.
        let codec = pick_codec();
        let req = gpu_proto::msg::Request::PresentFrame { memory: readback, len: (w * h * 4), seq, width: w as u16, height: h as u16, format, codec };
        st.stats.frames += 1;
        st.stats.t_wait_ns += t0.elapsed().as_nanos() as u64;
        crate::latency::wait_for_room(max_inflight_frames(), std::time::Duration::from_millis(100));
        crate::latency::sent(seq);
        return if st.client.send(&req).is_ok() { 0 } else { VK_ERROR_INITIALIZATION_FAILED };
    }
    if exec_named(st, "vkQueueSubmit", &mut [queue, 1, si.p(), fence]) < 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    if exec_named(st, "vkWaitForFences", &mut [dev, 1, &fence as *const u64 as u64, 1, WHOLE]) < 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    exec_named(st, "vkResetFences", &mut [dev, 1, &fence as *const u64 as u64]);
    let t1 = std::time::Instant::now();
    let size = w as usize * h as usize * 4;
    let Some(mut frame) = st.read_mem(readback, 0, size) else { return VK_ERROR_INITIALIZATION_FAILED };
    if matches!(fmt, 37 | 43) {
        let mut bgra = vec![0u8; size];
        if rgba_to_bgra(&frame, &mut bgra) {
            frame = bgra;
        }
    }
    let t2 = std::time::Instant::now();
    st.stats.t_wait_ns += (t1 - t0).as_nanos() as u64;
    st.stats.t_read_ns += (t2 - t1).as_nanos() as u64;
    st.stats.frames += 1;
    let Some(sw) = st.swapchains.get_mut(&sc) else { return VK_ERROR_OUT_OF_DATE };
    sw.frames += 1;
    if let (Ok(path), true) = (std::env::var("GPU_ICD_DUMP_FRAME"), sw.frames == 1) {
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        for p in frame.chunks(4) {
            ppm.extend([p[2], p[1], p[0]]);
        }
        let _ = std::fs::write(path, ppm);
    }
    if sw.presenter.is_none() {
        let (surface, w, h) = (sw.surface, sw.width, sw.height);
        match with_local(|l| Presenter::new(l, surface, w, h)) {
            Some(Ok(p)) => sw.presenter = Some(p),
            Some(Err(e)) => {
                eprintln!("gpu-icd: lokalny prezenter: {e}");
                return VK_ERROR_INITIALIZATION_FAILED;
            }
            None => return VK_ERROR_INITIALIZATION_FAILED,
        }
    }
    let Some(p) = sw.presenter.as_mut() else { return VK_ERROR_INITIALIZATION_FAILED };
    let r = match with_local(|l| p.present(l, &frame)) {
        Some(Ok(r)) => r,
        _ => VK_ERROR_INITIALIZATION_FAILED,
    };
    st.stats.t_local_ns += t2.elapsed().as_nanos() as u64;
    r
}

/// Ile klatek wolno mieć oddanych, a jeszcze niewyświetlonych (`GPU_ICD_MAX_INFLIGHT`, domyślnie 3; 0 wyłącza hamowanie).
/// Więcej oznacza gładszy FPS przy dużym RTT, mniej: mniejsze opóźnienie (kursor, sterowanie).
fn max_inflight_frames() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    match *N.get_or_init(|| std::env::var("GPU_ICD_MAX_INFLIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(3)) {
        0 => usize::MAX,
        n => n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(types: &[u32]) -> Vec<u8> {
        let mut p = vec![0u8; 536];
        p[16..20].copy_from_slice(&(types.len() as u32).to_le_bytes());
        for (i, f) in types.iter().enumerate() {
            p[20 + i * 8..24 + i * 8].copy_from_slice(&f.to_le_bytes());
        }
        p
    }

    #[test]
    fn memory_type_selection_respects_bits_and_required_flags() {
        let p = props(&[0x1, 0x6, 0x7, 0x2]);
        assert_eq!(pick_memory_type(&p, 0xF, 0x1), Some(0), "pierwszy device-local");
        assert_eq!(pick_memory_type(&p, 0xF, 0x6), Some(1), "pierwszy host-visible+coherent");
        assert_eq!(pick_memory_type(&p, 0b1100, 0x1), Some(2), "maska bitów wyklucza typ 0");
        assert_eq!(pick_memory_type(&p, 0b0001, 0x6), None);
        assert_eq!(pick_memory_type(&p, 0xF, 0), Some(0), "bez wymagań: pierwszy dozwolony");
        assert_eq!(pick_memory_type(&[0u8; 4], 0xF, 0), None, "za krótkie dane");
        assert_eq!(pick_memory_type(&props(&[]), 0xF, 0), None);
    }
}
