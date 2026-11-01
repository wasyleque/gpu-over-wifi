//! Logika odpowiedzi o powierzchni okna (WSI): co ogłaszamy i jak korygujemy odpowiedzi lokalnego Intela.
//! Funkcje czyste, bez Vulkana, więc w pełni testowalne.

/// Rozszerzenia instancji związane z powierzchnią: (nazwa, wersja specyfikacji).
pub const SURFACE_EXTENSIONS: &[(&str, u32)] = &[
    ("VK_KHR_surface", 25),
    ("VK_KHR_wayland_surface", 6),
    ("VK_KHR_xcb_surface", 6),
    ("VK_KHR_xlib_surface", 6),
    ("VK_EXT_headless_surface", 1),
];

/// Liczba obrazów wirtualnego swapchaina (min = max): 3 daje aplikacji płynność bez wzrostu opóźnienia.
pub const SWAPCHAIN_IMAGES: u32 = 3;

/// Użycia obrazów, które potrafi zdalna karta (obrazy żyją na serwerze): kopiowanie, próbkowanie, zapis, kolor.
pub const SUPPORTED_USAGE: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x80;

pub const PLATFORM_WAYLAND: u32 = 1;
pub const PLATFORM_XCB: u32 = 3;
pub const PLATFORM_XLIB: u32 = 4;
pub const PLATFORM_HEADLESS: u32 = 9;

/// Powierzchnia ICD w układzie zgodnym z `VkIcdSurface*` (platforma @0, dwa wskaźniki/uchwyty @8 i @16).
/// Taki sam układ rozumie sterownik lokalny, więc przekazujemy mu ją bez konwersji.
pub fn icd_surface(platform: u32, a: u64, b: u64) -> Box<[u64; 3]> {
    Box::new([platform as u64, a, b])
}

const VK_FORMAT_R8G8B8A8_UNORM: u32 = 37;
const VK_FORMAT_R8G8B8A8_SRGB: u32 = 43;
const VK_FORMAT_B8G8R8A8_UNORM: u32 = 44;
const VK_FORMAT_B8G8R8A8_SRGB: u32 = 50;
const VK_PRESENT_MODE_IMMEDIATE: u32 = 0;
const VK_PRESENT_MODE_FIFO: u32 = 2;

/// Rozszerzenia powierzchni, które ogłaszamy: przecięcie naszej listy z tym, co ma lokalny sterownik.
pub fn advertised_surface_extensions(local: &[String]) -> Vec<(&'static str, u32)> {
    if !local.iter().any(|e| e == "VK_KHR_surface") {
        return Vec::new();
    }
    SURFACE_EXTENSIONS.iter().filter(|(n, _)| local.iter().any(|l| l == n)).copied().collect()
}

/// Formaty powierzchni: przecięcie z formatami 8-bitowymi, które rozumie DXVK; kolejność B8G8R8A8 pierwsza.
/// Pusta odpowiedź lokalnego sterownika zostaje pusta (nie zmyślamy formatów, których okno nie wyświetli).
pub fn filter_formats(local: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let order = [VK_FORMAT_B8G8R8A8_UNORM, VK_FORMAT_B8G8R8A8_SRGB, VK_FORMAT_R8G8B8A8_UNORM, VK_FORMAT_R8G8B8A8_SRGB];
    let mut out = Vec::new();
    for f in order {
        out.extend(local.iter().filter(|(fmt, _)| *fmt == f).copied());
    }
    out
}

/// Tryby prezentacji: FIFO zawsze (wymagany przez specyfikację), IMMEDIATE tylko gdy ma go lokalny sterownik.
/// `MAILBOX` celowo nie jest ogłaszany: obiecuje semantykę, której nie dowieziemy przed pipeline'em klatek (M6).
pub fn filter_present_modes(local: &[u32]) -> Vec<u32> {
    let mut out = vec![VK_PRESENT_MODE_FIFO];
    if local.contains(&VK_PRESENT_MODE_IMMEDIATE) {
        out.insert(0, VK_PRESENT_MODE_IMMEDIATE);
    }
    out
}

/// Koryguje `VkSurfaceCapabilitiesKHR` (52 B) od lokalnego sterownika: liczba obrazów i obsługiwane użycia.
/// Zwraca `false`, gdy bufor ma zły rozmiar.
pub fn patch_capabilities(caps: &mut [u8]) -> bool {
    if caps.len() != 52 {
        return false;
    }
    caps[0..4].copy_from_slice(&SWAPCHAIN_IMAGES.to_le_bytes()); // minImageCount
    caps[4..8].copy_from_slice(&SWAPCHAIN_IMAGES.to_le_bytes()); // maxImageCount
    caps[48..52].copy_from_slice(&SUPPORTED_USAGE.to_le_bytes()); // supportedUsageFlags
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn icd_surface_has_the_vkicdsurface_layout() {
        let s = icd_surface(PLATFORM_WAYLAND, 0xD15, 0x5F);
        let p = s.as_ptr() as *const u8;
        unsafe {
            assert_eq!(std::ptr::read_unaligned(p as *const u32), 1, "platforma @0");
            assert_eq!(std::ptr::read_unaligned(p.add(8) as *const u64), 0xD15, "wl_display @8");
            assert_eq!(std::ptr::read_unaligned(p.add(16) as *const u64), 0x5F, "wl_surface @16");
        }
        let xcb = icd_surface(PLATFORM_XCB, 1, 7);
        // SAFETY: bufor ma 24 bajty; czytamy u32 pod offsetem 16.
        assert_eq!(unsafe { std::ptr::read_unaligned((xcb.as_ptr() as *const u8).add(16) as *const u32) }, 7, "okno XCB (u32) @16");
        assert_eq!(icd_surface(PLATFORM_HEADLESS, 0, 0)[0], 9);
    }

    #[test]
    fn extensions_require_the_base_surface_extension_and_keep_only_known_ones() {
        assert!(advertised_surface_extensions(&s(&["VK_KHR_wayland_surface"])).is_empty(), "bez VK_KHR_surface nic nie ogłaszamy");
        assert!(advertised_surface_extensions(&[]).is_empty());
        let got = advertised_surface_extensions(&s(&["VK_KHR_surface", "VK_KHR_wayland_surface", "VK_KHR_display", "VK_EXT_debug_utils"]));
        assert_eq!(got, vec![("VK_KHR_surface", 25), ("VK_KHR_wayland_surface", 6)]);
        let all = advertised_surface_extensions(&s(&["VK_EXT_headless_surface", "VK_KHR_xlib_surface", "VK_KHR_surface", "VK_KHR_xcb_surface", "VK_KHR_wayland_surface"]));
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].0, "VK_KHR_surface", "kolejność z naszej listy, nie sterownika");
    }

    #[test]
    fn formats_are_filtered_and_ordered_with_bgra_first() {
        let local = [(37, 0), (50, 0), (44, 0), (97, 0), (43, 7), (44, 4)];
        assert_eq!(filter_formats(&local), vec![(44, 0), (44, 4), (50, 0), (37, 0), (43, 7)]);
        assert!(filter_formats(&[(97, 0), (64, 0)]).is_empty(), "nie zmyślamy formatów");
        assert!(filter_formats(&[]).is_empty());
    }

    #[test]
    fn present_modes_always_have_fifo_and_never_mailbox() {
        assert_eq!(filter_present_modes(&[]), vec![2]);
        assert_eq!(filter_present_modes(&[2, 1, 0]), vec![0, 2], "MAILBOX (1) odrzucony, IMMEDIATE zachowany");
        assert_eq!(filter_present_modes(&[1]), vec![2]);
        assert_eq!(filter_present_modes(&[0]), vec![0, 2]);
    }

    #[test]
    fn capabilities_are_patched_in_place_and_wrong_sizes_are_refused() {
        let mut c = vec![0xAAu8; 52];
        c[8..12].copy_from_slice(&800u32.to_le_bytes()); // currentExtent.width zostaje
        assert!(patch_capabilities(&mut c));
        assert_eq!(u32::from_le_bytes(c[0..4].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(c[4..8].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(c[8..12].try_into().unwrap()), 800, "rozmiar okna od lokalnego sterownika");
        let usage = u32::from_le_bytes(c[48..52].try_into().unwrap());
        assert_eq!(usage & 0x10, 0x10, "COLOR_ATTACHMENT");
        assert_eq!(usage & 0x1, 0x1, "TRANSFER_SRC");
        assert!(!patch_capabilities(&mut [0u8; 51]));
        assert!(!patch_capabilities(&mut [0u8; 53]));
        assert!(!patch_capabilities(&mut []));
    }
}
