//! Lokalny sterownik do prezentacji (Intel laptopa), ładowany BEZPOŚREDNIO jako ICD, z pominięciem loadera.
//!
//! Powód: aplikacja używa loadera z `VK_DRIVER_FILES` wskazującym tylko nasz ICD, a drugi `dlopen` loadera
//! zwróciłby ten sam stan. Sterownik systemowy wołamy więc przez jego `vk_icdGetInstanceProcAddr`.
#![allow(dead_code)]

use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};

use gpu_renderd::vk::loader::{LibVulkan, VkInstance, VkPhysicalDevice};
use gpu_renderd::vk::replay::call_thunk;

const VK_API_1_1: u32 = 0x0040_1000;
/// Rozszerzenia instancji potrzebne do powierzchni okna (włączamy te, które sterownik ma).
const WANTED_INSTANCE_EXTS: &[&str] = &["VK_KHR_surface", "VK_KHR_wayland_surface", "VK_KHR_xcb_surface", "VK_KHR_xlib_surface", "VK_EXT_headless_surface"];

/// Wyciąga `library_path` z manifestu ICD (bez zależności od biblioteki JSON). Ścieżki względne z `/` liczymy
/// od katalogu manifestu, a goła nazwa pliku jest zostawiana dynamicznemu linkerowi.
pub fn library_path_from_manifest(json: &str, manifest_dir: &Path) -> Option<PathBuf> {
    let i = json.find("\"library_path\"")?;
    let rest = &json[i + "\"library_path\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let p = &rest[..end];
    if p.is_empty() {
        return None;
    }
    let path = PathBuf::from(p);
    if path.is_absolute() || !p.contains('/') {
        Some(path)
    } else {
        Some(manifest_dir.join(path))
    }
}

/// Manifesty lokalnych sterowników do wyświetlania, od najbardziej prawdopodobnych: Intel, AMD, NVIDIA, reszta fizycznych GPU.
/// Pomijamy sterowniki programowe i wirtualne (lvp, virtio, gfxstream) oraz nasz ICD. `GPU_ICD_LOCAL_ICD` wskazuje manifest wprost.
pub fn find_local_manifests() -> Vec<PathBuf> {
    if let Ok(p) = std::env::var("GPU_ICD_LOCAL_ICD") {
        return vec![PathBuf::from(p)];
    }
    let mut dirs = vec![PathBuf::from("/usr/share/vulkan/icd.d"), PathBuf::from("/etc/vulkan/icd.d"), PathBuf::from("/usr/local/share/vulkan/icd.d")];
    if let Ok(h) = std::env::var("HOME") {
        dirs.push(PathBuf::from(h).join(".local/share/vulkan/icd.d"));
    }
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let skip = ["lvp", "virtio", "gfxstream", "gpu_icd", "gpu-icd", "lavapipe"].iter().any(|x| name.contains(x));
            if name.ends_with(".json") && !skip {
                out.push(e.path());
            }
        }
    }
    let rank = |p: &PathBuf| {
        let n = p.to_string_lossy().to_lowercase();
        let base = if n.contains("intel") {
            0
        } else if n.contains("radeon") || n.contains("amd") {
            1
        } else if n.contains("nvidia") {
            2
        } else {
            3
        };
        (base, n.contains("hasvk"), p.clone())
    };
    out.sort_by_key(rank);
    out.dedup();
    out
}

pub struct LocalVulkan {
    lib: LibVulkan,
    pub instance: VkInstance,
    pub pdev: VkPhysicalDevice,
    pub device_name: String,
    pub api_version: u32,
    pub enabled_instance_exts: Vec<String>,
    pub device_exts: Vec<String>,
    pub manifest: PathBuf,
}

// SAFETY: surowe wskaźniki to uchwyty sterownika używane pod blokadą (jedna instancja w procesie).
unsafe impl Send for LocalVulkan {}

fn ext_names(buf: &[u8], n: usize) -> Vec<String> {
    (0..n)
        .filter_map(|i| {
            let e = buf.get(i * 260..i * 260 + 256)?;
            CStr::from_bytes_until_nul(e).ok().map(|c| c.to_string_lossy().into_owned())
        })
        .collect()
}

impl LocalVulkan {
    pub(crate) fn fnptr(lib: &LibVulkan, inst: VkInstance, name: &str) -> Option<*const c_void> {
        let c = CString::new(name).ok()?;
        // SAFETY: `inst` jest null albo ważną instancją tego sterownika.
        unsafe { lib.proc_addr(inst, &c) }
    }

    pub(crate) fn call(&self, name: &str, ints: &[u64]) -> Option<i32> {
        let f = Self::fnptr(&self.lib, self.instance, name)?;
        if std::env::var("GPU_ICD_DEBUG_LOCAL").is_ok() {
            eprintln!("[gpu-icd] lokalnie: {name}({ints:x?})");
        }
        // SAFETY: wskaźniki w `ints` żyją do końca wywołania; sygnatura zgodna z vk.xml.
        unsafe { call_thunk(f, ints, &[]) }.map(|r| r as u32 as i32)
    }

    /// Tworzy powierzchnię PRZEZ lokalny sterownik (`vkCreate*SurfaceKHR` z create info aplikacji). Sterownik (Mesa) wymaga
    /// własnego, większego obiektu powierzchni; ręcznie złożona struktura `VkIcdSurface*` wystarcza tylko do zapytań o możliwości.
    pub fn create_surface(&self, fn_name: &str, info: u64) -> Option<u64> {
        let mut out = 0u64;
        let r = self.call(fn_name, &[self.instance as u64, info, 0, &mut out as *mut u64 as u64])?;
        (r >= 0 && out != 0).then_some(out)
    }

    pub fn destroy_surface(&self, surface: u64) {
        self.call("vkDestroySurfaceKHR", &[self.instance as u64, surface, 0]);
    }

    /// Czy lokalna karta potrafi prezentować na `surface` z rodziny kolejek `qf`.
    pub fn surface_support(&self, qf: u32, surface: u64) -> Option<bool> {
        let mut out = 0u32;
        let r = self.call("vkGetPhysicalDeviceSurfaceSupportKHR", &[self.pdev as u64, qf as u64, surface, &mut out as *mut u32 as u64])?;
        (r >= 0).then_some(out != 0)
    }

    /// `VkSurfaceCapabilitiesKHR` (52 B) od lokalnego sterownika.
    pub fn surface_caps(&self, surface: u64) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 56];
        let r = self.call("vkGetPhysicalDeviceSurfaceCapabilitiesKHR", &[self.pdev as u64, surface, buf.as_mut_ptr() as u64])?;
        buf.truncate(52);
        (r >= 0).then_some(buf)
    }

    /// Formaty powierzchni jako pary (format, przestrzeń barw).
    pub fn surface_formats(&self, surface: u64) -> Option<Vec<(u32, u32)>> {
        let mut n = 0u32;
        self.call("vkGetPhysicalDeviceSurfaceFormatsKHR", &[self.pdev as u64, surface, &mut n as *mut u32 as u64, 0])?;
        let mut buf = vec![0u8; n as usize * 8];
        let r = self.call("vkGetPhysicalDeviceSurfaceFormatsKHR", &[self.pdev as u64, surface, &mut n as *mut u32 as u64, buf.as_mut_ptr() as u64])?;
        (r >= 0).then(|| buf.chunks(8).take(n as usize).map(|c| (u32::from_le_bytes([c[0], c[1], c[2], c[3]]), u32::from_le_bytes([c[4], c[5], c[6], c[7]]))).collect())
    }

    pub fn present_modes(&self, surface: u64) -> Option<Vec<u32>> {
        let mut n = 0u32;
        self.call("vkGetPhysicalDeviceSurfacePresentModesKHR", &[self.pdev as u64, surface, &mut n as *mut u32 as u64, 0])?;
        let mut buf = vec![0u8; n as usize * 4];
        let r = self.call("vkGetPhysicalDeviceSurfacePresentModesKHR", &[self.pdev as u64, surface, &mut n as *mut u32 as u64, buf.as_mut_ptr() as u64])?;
        (r >= 0).then(|| buf.chunks(4).take(n as usize).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    /// Czy lokalna karta może prezentować na danej platformie (`kind`: nazwa funkcji `vkGetPhysicalDevice*PresentationSupportKHR`).
    pub fn presentation_support(&self, kind: &str, qf: u32, a: u64, b: u64) -> bool {
        let f = Self::fnptr(&self.lib, self.instance, kind);
        let Some(f) = f else { return false };
        // SAFETY: argumenty to wskaźniki platformy przekazane przez aplikację (display/connection) i indeksy.
        unsafe { call_thunk(f, &[self.pdev as u64, qf as u64, a, b], &[]) }.is_some_and(|r| (r as u32) != 0)
    }

    /// Otwiera pierwszy działający sterownik Intela z manifestów i wybiera jego pierwsze urządzenie.
    pub fn open() -> Result<LocalVulkan, String> {
        let mut last = "nie znaleziono manifestu lokalnego sterownika Vulkana".to_string();
        for m in find_local_manifests() {
            match Self::open_manifest(&m) {
                Ok(l) => return Ok(l),
                Err(e) => last = format!("{}: {e}", m.display()),
            }
        }
        Err(last)
    }

    pub fn open_manifest(manifest: &Path) -> Result<LocalVulkan, String> {
        let json = std::fs::read_to_string(manifest).map_err(|e| e.to_string())?;
        let dir = manifest.parent().unwrap_or(Path::new("."));
        let lib_path = library_path_from_manifest(&json, dir).ok_or("brak library_path w manifeście")?;
        let lib = LibVulkan::load_from_symbol(&lib_path.to_string_lossy(), c"vk_icdGetInstanceProcAddr").map_err(|e| format!("{e:?}"))?;

        // Rozszerzenia instancji (funkcja globalna: instancja null).
        let list = Self::fnptr(&lib, std::ptr::null_mut(), "vkEnumerateInstanceExtensionProperties").ok_or("brak vkEnumerateInstanceExtensionProperties")?;
        let mut n = 0u32;
        // SAFETY: dwuwywołaniowy idiom z poprawnymi wskaźnikami wyjściowymi.
        unsafe { call_thunk(list, &[0, &mut n as *mut u32 as u64, 0], &[]) };
        let mut buf = vec![0u8; n as usize * 260];
        unsafe { call_thunk(list, &[0, &mut n as *mut u32 as u64, buf.as_mut_ptr() as u64], &[]) };
        let available = ext_names(&buf, n as usize);
        let enabled: Vec<String> = WANTED_INSTANCE_EXTS.iter().filter(|w| available.iter().any(|a| a == *w)).map(|s| s.to_string()).collect();

        let cnames: Vec<CString> = enabled.iter().filter_map(|s| CString::new(s.as_str()).ok()).collect();
        let ptrs: Vec<*const std::ffi::c_char> = cnames.iter().map(|c| c.as_ptr()).collect();
        let mut app = [0u64; 6];
        app[5] = (VK_API_1_1 as u64) << 32; // apiVersion @44 (dolne 32 bity słowa 5)
        let mut info = [0u64; 8];
        info[0] = 1; // VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO
        info[3] = app.as_ptr() as u64; // pApplicationInfo @24
        info[6] = ptrs.len() as u64; // enabledExtensionCount @48
        info[7] = ptrs.as_ptr() as u64; // ppEnabledExtensionNames @56
        let create = Self::fnptr(&lib, std::ptr::null_mut(), "vkCreateInstance").ok_or("brak vkCreateInstance")?;
        let mut inst: VkInstance = std::ptr::null_mut();
        // SAFETY: struktury i tablica nazw żyją do końca wywołania.
        let r = unsafe { call_thunk(create, &[info.as_ptr() as u64, 0, &mut inst as *mut VkInstance as u64], &[]) }.map(|r| r as u32 as i32).ok_or("thunk")?;
        if r != 0 || inst.is_null() {
            return Err(format!("vkCreateInstance = {r}"));
        }
        let mut me = LocalVulkan { lib, instance: inst, pdev: std::ptr::null_mut(), device_name: String::new(), api_version: 0, enabled_instance_exts: enabled, device_exts: Vec::new(), manifest: manifest.to_path_buf() };

        let mut count = 0u32;
        me.call("vkEnumeratePhysicalDevices", &[inst as u64, &mut count as *mut u32 as u64, 0]);
        if count == 0 {
            return Err("brak urządzeń fizycznych".into());
        }
        let mut pds = vec![std::ptr::null_mut::<c_void>(); count as usize];
        me.call("vkEnumeratePhysicalDevices", &[inst as u64, &mut count as *mut u32 as u64, pds.as_mut_ptr() as u64]);
        me.pdev = pds[0];
        // VkPhysicalDeviceProperties (824 B): apiVersion @0, deviceName @20.
        let mut props = vec![0u8; 832];
        me.call("vkGetPhysicalDeviceProperties", &[me.pdev as u64, props.as_mut_ptr() as u64]);
        me.api_version = u32::from_le_bytes([props[0], props[1], props[2], props[3]]);
        me.device_name = CStr::from_bytes_until_nul(&props[20..276]).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        let mut dn = 0u32;
        me.call("vkEnumerateDeviceExtensionProperties", &[me.pdev as u64, 0, &mut dn as *mut u32 as u64, 0]);
        let mut dbuf = vec![0u8; dn as usize * 260];
        me.call("vkEnumerateDeviceExtensionProperties", &[me.pdev as u64, 0, &mut dn as *mut u32 as u64, dbuf.as_mut_ptr() as u64]);
        me.device_exts = ext_names(&dbuf, dn as usize);
        Ok(me)
    }
}

impl Drop for LocalVulkan {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            self.call("vkDestroyInstance", &[self.instance as u64, 0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_path_forms() {
        let dir = Path::new("/usr/share/vulkan/icd.d");
        let bare = r#"{ "ICD": { "api_version": "1.4", "library_path": "libvulkan_intel.so" }, "file_format_version": "1.0.1" }"#;
        assert_eq!(library_path_from_manifest(bare, dir), Some(PathBuf::from("libvulkan_intel.so")));
        let rel = r#"{"ICD":{"library_path" : "../lib/x.so"}}"#;
        assert_eq!(library_path_from_manifest(rel, dir), Some(dir.join("../lib/x.so")));
        let abs = r#"{"ICD":{"library_path":"/opt/v/lib.so"}}"#;
        assert_eq!(library_path_from_manifest(abs, dir), Some(PathBuf::from("/opt/v/lib.so")));
        for bad in ["", "{}", r#"{"library_path": }"#, r#"{"library_path": ""}"#, r#"{"library_path": "x"#, r#"{"library_path" "x"}"#] {
            assert_eq!(library_path_from_manifest(bad, dir), None, "{bad:?}");
        }
    }

    #[test]
    fn manifests_skip_software_and_virtual_drivers_and_rank_intel_first_hasvk_last() {
        std::env::remove_var("GPU_ICD_LOCAL_ICD");
        let m = find_local_manifests();
        for p in &m {
            let n = p.to_string_lossy().to_lowercase();
            assert!(!n.contains("lvp") && !n.contains("virtio") && !n.contains("gfxstream") && !n.contains("gpu_icd"), "{n}");
        }
        let pos = |f: &dyn Fn(&str) -> bool| m.iter().position(|p| f(&p.to_string_lossy().to_lowercase()));
        if let (Some(i), Some(n)) = (pos(&|n| n.contains("intel_icd")), pos(&|n| n.contains("nvidia"))) {
            assert!(i < n, "Intel przed NVIDIA");
        }
        if let (Some(h), Some(i)) = (pos(&|n| n.contains("hasvk")), pos(&|n| n.contains("intel_icd"))) {
            assert!(i < h, "hasvk po nowszym intel_icd");
        }
    }

    #[test]
    fn opens_the_real_intel_driver_directly_and_sees_presentation_extensions() {
        let Ok(l) = LocalVulkan::open() else {
            eprintln!("POMINIĘTO: brak sterownika Intela w środowisku testowym");
            return;
        };
        assert!(!l.device_name.is_empty());
        assert!(l.api_version >= VK_API_1_1, "{:#x}", l.api_version);
        assert!(l.enabled_instance_exts.iter().any(|e| e == "VK_KHR_surface"), "{:?}", l.enabled_instance_exts);
        assert!(l.device_exts.iter().any(|e| e == "VK_KHR_swapchain"), "brak VK_KHR_swapchain");
        eprintln!("lokalny sterownik: {} ({}), instancja: {:?}", l.device_name, l.manifest.display(), l.enabled_instance_exts);
    }
}

#[cfg(test)]
mod experiments {
    use super::*;

    #[test]
    fn headless_surface_handle_as_the_loader_would_pass_it() {
        let Ok(l) = LocalVulkan::open() else { return };
        // VkIcdSurfaceHeadless { VkIcdSurfaceBase { platform } }: VK_ICD_WSI_PLATFORM_HEADLESS = 9.
        let surf = [9u64; 2];
        let h = surf.as_ptr() as u64;
        eprintln!("support: {:?}", l.surface_support(0, h));
        eprintln!("caps: {:?}", l.surface_caps(h).map(|c| (u32::from_le_bytes(c[0..4].try_into().unwrap()), u32::from_le_bytes(c[4..8].try_into().unwrap()))));
        eprintln!("formats: {:?}", l.surface_formats(h).map(|f| f.len()));
        eprintln!("modes: {:?}", l.present_modes(h));
        // Ta sama próba przez własną powierzchnię sterownika (vkCreateHeadlessSurfaceEXT wywołany bezpośrednio).
        let mut info = [0u64; 3];
        info[0] = 1_000_256_000;
        let mut own: u64 = 0;
        let r = l.call("vkCreateHeadlessSurfaceEXT", &[l.instance as u64, info.as_ptr() as u64, 0, &mut own as *mut u64 as u64]);
        eprintln!("własna powierzchnia headless: wynik {r:?}, uchwyt {own:#x}");
        if r == Some(0) {
            eprintln!("  support {:?}, caps {:?}", l.surface_support(0, own), l.surface_caps(own).map(|c| c.len()));
        }
    }
}
