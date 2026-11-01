//! Lokalna prezentacja klatek: urządzenie i swapchain na Intelu (sterownik ICD wołany bezpośrednio), bufor pośredni
//! i kopia `vkCmdCopyBufferToImage` + `vkQueuePresentKHR`. Naiwna wersja (czeka na fence po każdej klatce).
#![allow(dead_code)]

use crate::local::LocalVulkan;
use crate::sbuild::S;

const WHOLE: u64 = u64::MAX;
const FMT_B8G8R8A8_UNORM: u32 = 44;
const LAYOUT_TRANSFER_DST: u32 = 7;
const LAYOUT_PRESENT_SRC: u32 = 1_000_001_002;

pub struct Presenter {
    dev: u64,
    queue: u64,
    swapchain: u64,
    staging: u64,
    staging_mem: u64,
    staging_ptr: *mut u8,
    cb: u64,
    sem_acquire: u64,
    sem_finish: u64,
    fence: u64,
    images: Vec<u64>,
    pub width: u32,
    pub height: u32,
}

// SAFETY: wskaźniki to uchwyty lokalnego sterownika i odwzorowanie pamięci używane pod blokadą (jeden prezenter na swapchain).
unsafe impl Send for Presenter {}

/// Kopiuje klatkę RGBA do BGRA (zamiana kanałów R i B); długości muszą być równe i podzielne przez 4.
pub fn rgba_to_bgra(src: &[u8], dst: &mut [u8]) -> bool {
    if src.len() != dst.len() || !src.len().is_multiple_of(4) {
        return false;
    }
    for (s, d) in src.chunks(4).zip(dst.chunks_mut(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = s[3];
    }
    true
}

fn ck(name: &str, r: Option<i32>) -> Result<i32, String> {
    match r {
        Some(c) if c >= 0 => Ok(c),
        Some(c) => Err(format!("{name} = {c}")),
        None => Err(format!("{name}: brak funkcji w sterowniku lokalnym")),
    }
}

impl Presenter {
    /// Tworzy device i swapchain (BGRA8, FIFO) na `surface` (uchwyt w układzie VkIcdSurface) dla okna `width` x `height`.
    pub fn new(l: &LocalVulkan, surface: u64, width: u32, height: u32) -> Result<Presenter, String> {
        let pd = l.pdev as u64;
        let family = 0u64;
        let mut sup = 0u32;
        ck("vkGetPhysicalDeviceSurfaceSupportKHR", l.call("vkGetPhysicalDeviceSurfaceSupportKHR", &[pd, family, surface, &mut sup as *mut u32 as u64]))?;
        if sup == 0 {
            return Err("lokalna rodzina kolejek 0 nie wspiera prezentacji na tej powierzchni".into());
        }
        let prio = [1.0f32];
        let mut qci = S::new("VkDeviceQueueCreateInfo");
        qci.u32("queueFamilyIndex", family as u32).u32("queueCount", 1).u64("pQueuePriorities", prio.as_ptr() as u64);
        let ext = std::ffi::CString::new("VK_KHR_swapchain").map_err(|e| e.to_string())?;
        let exts = [ext.as_ptr()];
        let mut dci = S::new("VkDeviceCreateInfo");
        dci.u32("queueCreateInfoCount", 1).u64("pQueueCreateInfos", qci.p()).u32("enabledExtensionCount", 1).u64("ppEnabledExtensionNames", exts.as_ptr() as u64);
        let mut dev = 0u64;
        ck("vkCreateDevice", l.call("vkCreateDevice", &[pd, dci.p(), 0, &mut dev as *mut u64 as u64]))?;
        let mut me = Presenter { dev, queue: 0, swapchain: 0, staging: 0, staging_mem: 0, staging_ptr: std::ptr::null_mut(), cb: 0, sem_acquire: 0, sem_finish: 0, fence: 0, images: Vec::new(), width, height };
        l.call("vkGetDeviceQueue", &[dev, family, 0, &mut me.queue as *mut u64 as u64]);

        let mut caps = vec![0u8; 56];
        ck("surface caps", l.call("vkGetPhysicalDeviceSurfaceCapabilitiesKHR", &[pd, surface, caps.as_mut_ptr() as u64]))?;
        let min_images = u32::from_le_bytes([caps[0], caps[1], caps[2], caps[3]]).max(2);
        let transform = u32::from_le_bytes([caps[40], caps[41], caps[42], caps[43]]);
        let mut sci = S::new("VkSwapchainCreateInfoKHR");
        sci.u64("surface", surface).u32("minImageCount", min_images).u32("imageFormat", FMT_B8G8R8A8_UNORM).u32("imageColorSpace", 0);
        sci.u32o("imageExtent", 0, width).u32o("imageExtent", 4, height).u32("imageArrayLayers", 1).u32("imageUsage", 0x2);
        sci.u32("preTransform", if transform == 0 { 1 } else { transform }).u32("compositeAlpha", 1).u32("presentMode", 2).u32("clipped", 1);
        ck("vkCreateSwapchainKHR", l.call("vkCreateSwapchainKHR", &[dev, sci.p(), 0, &mut me.swapchain as *mut u64 as u64]))?;
        let mut n = 0u32;
        l.call("vkGetSwapchainImagesKHR", &[dev, me.swapchain, &mut n as *mut u32 as u64, 0]);
        me.images = vec![0u64; n as usize];
        l.call("vkGetSwapchainImagesKHR", &[dev, me.swapchain, &mut n as *mut u32 as u64, me.images.as_mut_ptr() as u64]);

        // Bufor pośredni (widoczny dla hosta) na jedną klatkę.
        let size = width as u64 * height as u64 * 4;
        let mut bci = S::new("VkBufferCreateInfo");
        bci.u64("size", size).u32("usage", 0x1);
        ck("vkCreateBuffer", l.call("vkCreateBuffer", &[dev, bci.p(), 0, &mut me.staging as *mut u64 as u64]))?;
        let mut req = S::new("VkMemoryRequirements");
        l.call("vkGetBufferMemoryRequirements", &[dev, me.staging, req.p()]);
        let rb = req.bytes();
        let (rsize, bits) = (u64::from_le_bytes(rb[0..8].try_into().unwrap_or([0; 8])), u32::from_le_bytes(rb[16..20].try_into().unwrap_or([0; 4])));
        let mut mem = vec![0u8; 520];
        l.call("vkGetPhysicalDeviceMemoryProperties", &[pd, mem.as_mut_ptr() as u64]);
        let count = u32::from_le_bytes([mem[0], mem[1], mem[2], mem[3]]) as usize;
        let mt = (0..count.min(32)).find(|&i| bits & (1 << i) != 0 && u32::from_le_bytes([mem[4 + i * 8], mem[5 + i * 8], mem[6 + i * 8], mem[7 + i * 8]]) & 0x6 == 0x6).ok_or("brak pamięci widocznej dla hosta (lokalnie)")?;
        let mut ai = S::new("VkMemoryAllocateInfo");
        ai.u64("allocationSize", rsize).u32("memoryTypeIndex", mt as u32);
        ck("vkAllocateMemory", l.call("vkAllocateMemory", &[dev, ai.p(), 0, &mut me.staging_mem as *mut u64 as u64]))?;
        ck("vkBindBufferMemory", l.call("vkBindBufferMemory", &[dev, me.staging, me.staging_mem, 0]))?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        ck("vkMapMemory", l.call("vkMapMemory", &[dev, me.staging_mem, 0, WHOLE, 0, &mut ptr as *mut *mut u8 as u64]))?;
        me.staging_ptr = ptr;

        let mut pci = S::new("VkCommandPoolCreateInfo");
        pci.u32("flags", 0x2).u32("queueFamilyIndex", family as u32);
        let mut pool = 0u64;
        ck("vkCreateCommandPool", l.call("vkCreateCommandPool", &[dev, pci.p(), 0, &mut pool as *mut u64 as u64]))?;
        let mut cai = S::new("VkCommandBufferAllocateInfo");
        cai.u64("commandPool", pool).u32("commandBufferCount", 1);
        ck("vkAllocateCommandBuffers", l.call("vkAllocateCommandBuffers", &[dev, cai.p(), &mut me.cb as *mut u64 as u64]))?;
        let sci2 = S::new("VkSemaphoreCreateInfo");
        ck("vkCreateSemaphore", l.call("vkCreateSemaphore", &[dev, sci2.p(), 0, &mut me.sem_acquire as *mut u64 as u64]))?;
        ck("vkCreateSemaphore", l.call("vkCreateSemaphore", &[dev, sci2.p(), 0, &mut me.sem_finish as *mut u64 as u64]))?;
        let fci = S::new("VkFenceCreateInfo");
        ck("vkCreateFence", l.call("vkCreateFence", &[dev, fci.p(), 0, &mut me.fence as *mut u64 as u64]))?;
        Ok(me)
    }

    /// Wyświetla klatkę BGRA (`width * height * 4` B, ciasno upakowaną). Zwraca kod z `vkQueuePresentKHR`
    /// (0 = sukces, 1000001003 = SUBOPTIMAL). Czeka na zakończenie, więc po powrocie bufor można użyć ponownie.
    pub fn present(&mut self, l: &LocalVulkan, bgra: &[u8]) -> Result<i32, String> {
        let size = self.width as usize * self.height as usize * 4;
        if bgra.len() != size {
            return Err(format!("zły rozmiar klatki: {} B zamiast {size}", bgra.len()));
        }
        let dev = self.dev;
        let mut idx = 0u32;
        ck("vkAcquireNextImageKHR", l.call("vkAcquireNextImageKHR", &[dev, self.swapchain, WHOLE, self.sem_acquire, 0, &mut idx as *mut u32 as u64]))?;
        let image = *self.images.get(idx as usize).ok_or("indeks obrazu poza zakresem")?;
        // SAFETY: odwzorowanie ma co najmniej `size` bajtów (rozmiar bufora = rozmiar klatki).
        unsafe { std::ptr::copy_nonoverlapping(bgra.as_ptr(), self.staging_ptr, size) };

        let barrier = |old: u32, new: u32, src: u32, dst: u32| {
            let mut b = S::new("VkImageMemoryBarrier");
            b.u32("srcAccessMask", src).u32("dstAccessMask", dst).u32("oldLayout", old).u32("newLayout", new);
            b.u32("srcQueueFamilyIndex", u32::MAX).u32("dstQueueFamilyIndex", u32::MAX).u64("image", image);
            b.u32o("subresourceRange", 0, 1).u32o("subresourceRange", 8, 1).u32o("subresourceRange", 16, 1);
            b
        };
        let to_dst = barrier(0, LAYOUT_TRANSFER_DST, 0, 0x1000);
        let to_present = barrier(LAYOUT_TRANSFER_DST, LAYOUT_PRESENT_SRC, 0x1000, 0);
        let mut copy = S::new("VkBufferImageCopy");
        copy.u32o("imageSubresource", 0, 1).u32o("imageSubresource", 12, 1).u32o("imageExtent", 0, self.width).u32o("imageExtent", 4, self.height).u32o("imageExtent", 8, 1);
        let mut begin = S::new("VkCommandBufferBeginInfo");
        begin.u32("flags", 1);
        let cb = self.cb;
        l.call("vkResetCommandBuffer", &[cb, 0]);
        ck("vkBeginCommandBuffer", l.call("vkBeginCommandBuffer", &[cb, begin.p()]))?;
        l.call("vkCmdPipelineBarrier", &[cb, 0x1, 0x1000, 0, 0, 0, 0, 0, 1, to_dst.p()]);
        l.call("vkCmdCopyBufferToImage", &[cb, self.staging, image, LAYOUT_TRANSFER_DST as u64, 1, copy.p()]);
        l.call("vkCmdPipelineBarrier", &[cb, 0x1000, 0x2000, 0, 0, 0, 0, 0, 1, to_present.p()]);
        ck("vkEndCommandBuffer", l.call("vkEndCommandBuffer", &[cb]))?;
        let mask = [0x1000u32];
        let mut sub = S::new("VkSubmitInfo");
        sub.u32("waitSemaphoreCount", 1).u64("pWaitSemaphores", &self.sem_acquire as *const u64 as u64).u64("pWaitDstStageMask", mask.as_ptr() as u64);
        sub.u32("commandBufferCount", 1).u64("pCommandBuffers", &cb as *const u64 as u64).u32("signalSemaphoreCount", 1).u64("pSignalSemaphores", &self.sem_finish as *const u64 as u64);
        ck("vkQueueSubmit", l.call("vkQueueSubmit", &[self.queue, 1, sub.p(), self.fence]))?;
        let mut pi = S::new("VkPresentInfoKHR");
        pi.u32("waitSemaphoreCount", 1).u64("pWaitSemaphores", &self.sem_finish as *const u64 as u64).u32("swapchainCount", 1);
        pi.u64("pSwapchains", &self.swapchain as *const u64 as u64).u64("pImageIndices", &idx as *const u32 as u64);
        let r = ck("vkQueuePresentKHR", l.call("vkQueuePresentKHR", &[self.queue, pi.p()]))?;
        ck("vkWaitForFences", l.call("vkWaitForFences", &[dev, 1, &self.fence as *const u64 as u64, 1, WHOLE]))?;
        l.call("vkResetFences", &[dev, 1, &self.fence as *const u64 as u64]);
        Ok(r)
    }

    /// Niszczy obiekty prezentera (po `vkDeviceWaitIdle`).
    pub fn destroy(&mut self, l: &LocalVulkan) {
        let dev = self.dev;
        l.call("vkDeviceWaitIdle", &[dev]);
        l.call("vkDestroyFence", &[dev, self.fence, 0]);
        l.call("vkDestroySemaphore", &[dev, self.sem_acquire, 0]);
        l.call("vkDestroySemaphore", &[dev, self.sem_finish, 0]);
        l.call("vkDestroyBuffer", &[dev, self.staging, 0]);
        l.call("vkFreeMemory", &[dev, self.staging_mem, 0]);
        l.call("vkDestroySwapchainKHR", &[dev, self.swapchain, 0]);
        l.call("vkDestroyDevice", &[dev, 0]);
        self.dev = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsi::{icd_surface, PLATFORM_HEADLESS};

    #[test]
    fn rgba_to_bgra_swaps_red_and_blue() {
        let src = [1u8, 2, 3, 4, 10, 20, 30, 40];
        let mut dst = [0u8; 8];
        assert!(rgba_to_bgra(&src, &mut dst));
        assert_eq!(dst, [3, 2, 1, 4, 30, 20, 10, 40]);
        assert!(!rgba_to_bgra(&src, &mut [0u8; 4]), "różne długości");
        assert!(!rgba_to_bgra(&[1, 2, 3], &mut [0u8; 3]), "długość niepodzielna przez 4");
    }

    #[test]
    fn presents_frames_on_a_headless_surface_with_the_real_local_driver() {
        let Ok(l) = LocalVulkan::open() else {
            eprintln!("POMINIĘTO: brak sterownika Intela");
            return;
        };
        if !l.enabled_instance_exts.iter().any(|e| e == "VK_EXT_headless_surface") {
            eprintln!("POMINIĘTO: sterownik bez VK_EXT_headless_surface");
            return;
        }
        let surface = icd_surface(PLATFORM_HEADLESS, 0, 0);
        let sh = surface.as_ptr() as u64;
        let (w, h) = (64u32, 48u32);
        let mut p = Presenter::new(&l, sh, w, h).expect("prezenter");
        for n in 0..5u8 {
            let frame: Vec<u8> = (0..w * h * 4).map(|i| (i as u8).wrapping_add(n)).collect();
            let r = p.present(&l, &frame).expect("present");
            assert!(r == 0 || r == 1_000_001_003, "kod prezentacji {r}");
        }
        assert!(p.present(&l, &[0u8; 10]).is_err(), "zły rozmiar klatki");
        p.destroy(&l);
    }
}
