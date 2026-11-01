//! Które rozszerzenia urządzenia ICD może ogłosić aplikacji. Ogłoszenie rozszerzenia, którego polecenie nie ma punktu
//! wejścia, daje null w loaderze i awarię (tak padł `vulkaninfo` przy Vulkan Video), więc rozszerzenie przechodzi tylko wtedy,
//! gdy KAŻDE jego polecenie przekazujemy generycznie, a jego działanie nie zależy od zasobów systemu lokalnego.

use gpu_proto::ext_table::{COMMAND_ALIASES, DEVICE_EXTENSIONS};
use gpu_proto::meta::find_command;

/// `graphics_pipeline_library`: sterownik NVIDII na serwerze pada (SIGSEGV w libnvidia-eglcore) przy `vkCreateGraphicsPipelines`
/// z `VkGraphicsPipelineLibraryCreateInfoEXT` (Dota 2); bez rozszerzenia gry używają zwykłych potoków.
/// Fragmenty nazw rozszerzeń, które wymagają obiektów systemu operacyjnego lub lokalnego wyświetlania albo mają semantykę
/// zależną od maszyny (uchwyty plików/dma-buf, multi-GPU, zegary hosta, wideo, śledzenie ray tracingu bez testów).
const DENY: &[&str] = &[
    "external_memory", "external_semaphore", "external_fence", "win32", "xlib", "xcb", "wayland", "android", "ggp", "fuchsia", "metal", "qnx",
    "display", "swapchain", "present", "surface", "full_screen", "drm", "video_", "device_group", "calibrated_timestamps",
    "performance_query", "acquire_xlib", "sci_sync", "ohos", "headless", "pipeline_binary", "cuda", "nvx_", "image_compression_control",
    "device_generated_commands", "shader_object", "opacity_micromap", "ray_tracing", "acceleration_structure", "ray_query",
    "deferred_host_operations", "graphics_pipeline_library", "frame_boundary", "low_latency", "anti_lag", "debug_report", "validation",
];

/// Czy polecenie jest wykonywane lokalnie przez ICD, a nie przekazywane (te nazwy zwracają null w `proc_addr_by_name`).
pub fn is_local_only_command(name: &str) -> bool {
    name.contains("Surface") || name.contains("Display") || name.contains("Swapchain") || name.contains("Present")
}

/// Nazwa polecenia po rozwiązaniu aliasu (`vkTrimCommandPoolKHR` -> `vkTrimCommandPool`); nazwa bez aliasu zostaje.
pub fn resolve_alias(name: &str) -> &str {
    let mut cur = name;
    // Łańcuch aliasów jest krótki; limit chroni przed cyklem w danych.
    for _ in 0..4 {
        match COMMAND_ALIASES.binary_search_by(|(a, _)| (*a).cmp(cur)) {
            Ok(i) => cur = COMMAND_ALIASES[i].1,
            Err(_) => break,
        }
    }
    cur
}

/// `true`, gdy rozszerzenie można ogłosić. `VK_KHR_swapchain` obsługuje ICD lokalnie (wirtualny swapchain).
pub fn is_advertisable(ext: &str) -> bool {
    if ext == "VK_KHR_swapchain" {
        return true;
    }
    let lower = ext.to_ascii_lowercase();
    if DENY.iter().any(|d| lower.contains(d)) {
        return false;
    }
    let Ok(i) = DEVICE_EXTENSIONS.binary_search_by(|(n, _)| (*n).cmp(ext)) else { return false };
    DEVICE_EXTENSIONS[i].1.iter().all(|c| !is_local_only_command(c) && find_command(resolve_alias(c)).is_some_and(|(_, m)| m.supported))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swapchain_is_always_advertised_and_wsi_or_os_extensions_never() {
        assert!(is_advertisable("VK_KHR_swapchain"));
        for e in ["VK_KHR_win32_surface", "VK_KHR_external_memory_fd", "VK_KHR_display", "VK_KHR_video_decode_h264", "VK_EXT_image_drm_format_modifier", "VK_KHR_present_wait", "VK_KHR_device_group", "VK_EXT_graphics_pipeline_library"] {
            assert!(!is_advertisable(e), "{e}");
        }
        assert!(!is_advertisable("VK_NIE_ISTNIEJE"));
    }

    #[test]
    fn aliases_resolve_to_core_commands() {
        assert_eq!(resolve_alias("vkTrimCommandPoolKHR"), "vkTrimCommandPool");
        assert_eq!(resolve_alias("vkTrimCommandPool"), "vkTrimCommandPool");
        assert_eq!(resolve_alias("vkNieMaTakiego"), "vkNieMaTakiego");
    }

    #[test]
    fn commandless_and_fully_forwardable_extensions_pass() {
        // Rozszerzenia tylko z funkcjami/strukturami (bez poleceń) i takie z poleceniami w tablicy metadanych.
        for e in ["VK_KHR_maintenance1", "VK_EXT_robustness2", "VK_KHR_synchronization2", "VK_KHR_dynamic_rendering", "VK_EXT_extended_dynamic_state"] {
            assert!(is_advertisable(e), "{e} powinno przejść: {:?}", DEVICE_EXTENSIONS.iter().find(|(n, _)| *n == e).map(|(_, c)| c.iter().filter(|x| find_command(resolve_alias(x)).is_none()).collect::<Vec<_>>()));
        }
    }

    #[test]
    fn every_advertisable_extension_has_all_commands_in_the_table() {
        let ok: Vec<&str> = DEVICE_EXTENSIONS.iter().map(|(n, _)| *n).filter(|n| *n != "VK_KHR_swapchain" && is_advertisable(n)).collect();
        assert!(ok.len() > 50, "ogłaszalnych rozszerzeń: {}", ok.len());
        for n in &ok {
            let (_, cmds) = DEVICE_EXTENSIONS.iter().find(|(m, _)| m == n).unwrap();
            assert!(cmds.iter().all(|c| find_command(resolve_alias(c)).is_some() && !is_local_only_command(c)), "{n}");
        }
        eprintln!("ogłaszalne: {}", ok.len());
    }
}
