//! Prawdziwy backend `GpuBackend` na loaderze Vulkana. Klient widzi wyłącznie nieprzejrzyste uchwyty
//! z `HandleTable`; adresy serwera nie opuszczają procesu, a łańcuchy `pNext` budujemy sami.
//!
//! Format `Response::Data` z `get_physical_device` (warianty `*2`): struktura główna (rozmiar z tablicy sType,
//! `pNext` wyzerowany), po niej bajty łańcucha (`ChainBuf::into_response_bytes`). Dla `QueueFamilyProperties2`:
//! przy `count == 0` cztery bajty LE z liczbą rodzin, inaczej `n` bloków (struktura + jej łańcuch) jeden po drugim.
//! Warianty bez `2` (`Properties`, `Features`...) nie są obsługiwane: sterownik ICD składa je z wersji `*2`.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::Arc;

use gpu_proto::msg::PdFunc;
use gpu_proto::stype::lookup;

use crate::handler::DeltaItem;
use crate::handler::{GpuBackend, ERR_UNKNOWN};
use crate::vk::chain::{build_chain, PNEXT_OFFSET};
use crate::vk::handles::{Handle, HandleTable};
use crate::vk::registry::Entry;
use crate::vk::replay::Replayer;
use crate::vk::loader::{
    GlobalFns, InstanceFns, LibVulkan, VkExtensionProperties, VkInstance, VkPhysicalDevice, GLOBAL_CALLS,
};

pub const VK_ERROR_EXTENSION_NOT_PRESENT: i32 = -7;
pub const VK_ERROR_FEATURE_NOT_PRESENT: i32 = -8;
pub const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;

const ST_APPLICATION_INFO: u32 = 0;
const ST_INSTANCE_CREATE_INFO: u32 = 1;
const ST_FEATURES2: u32 = 1_000_059_000;
const ST_PROPERTIES2: u32 = 1_000_059_001;
const ST_FORMAT_PROPERTIES2: u32 = 1_000_059_002;
const ST_QUEUE_FAMILY_PROPERTIES2: u32 = 1_000_059_005;
const ST_MEMORY_PROPERTIES2: u32 = 1_000_059_006;

const MAX_INSTANCES: usize = 64;
const MAX_PHYSICAL_DEVICES: usize = 1024;
/// Górny limit liczby elementów zwracanych przez zapytania o tablice (poza limitem protokołu).
const MAX_ARRAY: u32 = 1024;

struct InstanceEntry {
    raw: VkInstance,
    fns: InstanceFns,
    pdevs: Vec<Handle>,
}

struct PdevEntry {
    raw: VkPhysicalDevice,
    instance: Handle,
}

impl VulkanBackend {
    /// Liczba uchwytów zarejestrowanych przez klienta (do testów wycieków rejestru).
    pub fn registered_handles(&self) -> usize {
        self.replay.registered_handles()
    }
}

pub struct VulkanBackend {
    lib: Arc<LibVulkan>,
    global: GlobalFns,
    instances: HandleTable<InstanceEntry>,
    pdevs: HandleTable<PdevEntry>,
    replay: Replayer,
}

// SAFETY: surowe wskaźniki to uchwyty Vulkana, których funkcje instancyjne wolno wołać z dowolnego wątku
// (zewnętrzna synchronizacja dotyczy tylko jednego obiektu naraz, a sesja ma wyłączny dostęp do swoich).
unsafe impl Send for VulkanBackend {}

/// Instancja trzymana do końca procesu. Loader Vulkana wyładowuje biblioteki ICD, gdy znika ostatnia instancja, a
/// sterownik NVIDIA po kilku takich cyklach ładowania/wyładowania przestaje widzieć kartę (zostaje llvmpipe). Zawsze żywa
/// instancja sprawia, że ICD ładuje się raz.
fn ensure_keeper_instance(global: &GlobalFns) {
    static KEEPER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    KEEPER.get_or_init(|| {
        let Ok(_g) = GLOBAL_CALLS.lock() else { return 0 };
        let mut info = Words::zeroed(64);
        write_u32(info.bytes_mut(), 0, ST_INSTANCE_CREATE_INFO);
        let mut raw: VkInstance = std::ptr::null_mut();
        // SAFETY: minimalna poprawna VkInstanceCreateInfo (bez rozszerzeń i warstw); wynik celowo nigdy nie jest niszczony.
        let r = unsafe { (global.create_instance)(info.ptr() as *const c_void, std::ptr::null(), &mut raw) };
        if r >= 0 { raw as usize } else { 0 }
    });
}

/// Zerwane połączenie (np. zabity klient) nie przechodzi przez `vkDestroyInstance`: sterownik NVIDIA po kilkunastu
/// nieposprzątanych instancjach przestaje widzieć kartę (zostaje tylko llvmpipe), więc sprzątamy wszystko przy zamknięciu sesji.
impl Drop for VulkanBackend {
    fn drop(&mut self) {
        self.replay.destroy_devices();
        for e in self.instances.drain_all() {
            // SAFETY: instancja została wyjęta z tablicy, więc nikt jej już nie użyje; urządzenia zniszczone powyżej.
            unsafe { (e.fns.destroy_instance)(e.raw, std::ptr::null()) };
        }
    }
}

fn vk_ok(r: i32) -> Result<(), i32> {
    if r >= 0 {
        Ok(())
    } else {
        Err(r)
    }
}

fn ext_name(e: &VkExtensionProperties) -> Option<String> {
    CStr::from_bytes_until_nul(&e.extension_name).ok().map(|c| c.to_string_lossy().into_owned())
}

fn write_u32(buf: &mut [u8], off: usize, v: u32) {
    if let Some(d) = buf.get_mut(off..off + 4) {
        d.copy_from_slice(&v.to_le_bytes());
    }
}

fn write_u64(buf: &mut [u8], off: usize, v: u64) {
    if let Some(d) = buf.get_mut(off..off + 8) {
        d.copy_from_slice(&v.to_le_bytes());
    }
}

/// Bufor `u64` jako bajty (wyrównanie 8), do budowy struktur przekazywanych do Vulkana.
struct Words(Vec<u64>);

impl Words {
    fn zeroed(bytes: usize) -> Words {
        Words(vec![0u64; bytes.div_ceil(8).max(1)])
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: `u64` nie ma wypełnienia, długość w bajtach to 8 * liczba słów.
        unsafe { std::slice::from_raw_parts_mut(self.0.as_mut_ptr() as *mut u8, self.0.len() * 8) }
    }
    fn bytes(&self) -> &[u8] {
        // SAFETY: jak wyżej, tylko do odczytu.
        unsafe { std::slice::from_raw_parts(self.0.as_ptr() as *const u8, self.0.len() * 8) }
    }
    fn ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr() as *mut c_void
    }
}

impl VulkanBackend {
    pub fn new() -> Result<VulkanBackend, i32> {
        let lib = Arc::new(LibVulkan::load().map_err(|_| VK_ERROR_INITIALIZATION_FAILED)?);
        let global = GlobalFns::load(&lib).map_err(|_| VK_ERROR_INITIALIZATION_FAILED)?;
        ensure_keeper_instance(&global);
        Ok(VulkanBackend {
            replay: Replayer::new(lib.clone()),
            lib,
            global,
            instances: HandleTable::with_tag(MAX_INSTANCES, 1),
            pdevs: HandleTable::with_tag(MAX_PHYSICAL_DEVICES, 2),
        })
    }

    fn list_instance_extensions(&self) -> Result<Vec<(String, u32)>, i32> {
        let _g = GLOBAL_CALLS.lock().map_err(|_| VK_ERROR_INITIALIZATION_FAILED)?;
        let mut n = 0u32;
        // SAFETY: wskaźniki wyjściowe wskazują na żywe zmienne/bufory o deklarowanej pojemności.
        vk_ok(unsafe { (self.global.enumerate_instance_extension_properties)(std::ptr::null(), &mut n, std::ptr::null_mut()) })?;
        let mut v = vec![VkExtensionProperties { extension_name: [0; 256], spec_version: 0 }; n as usize];
        vk_ok(unsafe { (self.global.enumerate_instance_extension_properties)(std::ptr::null(), &mut n, v.as_mut_ptr()) })?;
        Ok(v.iter().take(n as usize).filter_map(|e| ext_name(e).map(|s| (s, e.spec_version))).collect())
    }

    fn pdev(&self, h: u64) -> Result<(VkPhysicalDevice, InstanceFns), i32> {
        let p = self.pdevs.get(Handle(h)).ok_or(ERR_UNKNOWN)?;
        let i = self.instances.get(p.instance).ok_or(ERR_UNKNOWN)?;
        Ok((p.raw, i.fns))
    }

    /// Jedno wywołanie `vkGetPhysicalDevice*2` dla struktury głównej `head_stype` z łańcuchem `chain`.
    fn query_head(&self, pdev: u64, head_stype: u32, format: u32, chain: &[u32]) -> Result<Vec<u8>, i32> {
        let (raw, fns) = self.pdev(pdev)?;
        let head = lookup(head_stype).ok_or(ERR_UNKNOWN)?;
        let mut cb = build_chain(chain).map_err(|_| VK_ERROR_FEATURE_NOT_PRESENT)?;
        let mut w = Words::zeroed(head.size as usize);
        write_u32(w.bytes_mut(), 0, head_stype);
        write_u64(w.bytes_mut(), PNEXT_OFFSET, cb.head_ptr() as usize as u64);
        let p = w.ptr();
        // SAFETY: `p` wskazuje na zainicjowaną strukturę o rozmiarze z tablicy sType, a jej łańcuch `pNext`
        // składa się z naszych buforów, które żyją do końca wywołania.
        unsafe {
            match head_stype {
                ST_PROPERTIES2 => (fns.get_properties2)(raw, p),
                ST_FEATURES2 => (fns.get_features2)(raw, p),
                ST_MEMORY_PROPERTIES2 => (fns.get_memory_properties2)(raw, p),
                ST_FORMAT_PROPERTIES2 => (fns.get_format_properties2)(raw, format, p),
                _ => return Err(ERR_UNKNOWN),
            }
        }
        let mut out = w.bytes().get(..head.size as usize).ok_or(ERR_UNKNOWN)?.to_vec();
        write_u64(&mut out, PNEXT_OFFSET, 0);
        out.extend(cb.into_response_bytes());
        Ok(out)
    }

    fn query_queue_families(&self, pdev: u64, count: u32, chain: &[u32]) -> Result<Vec<u8>, i32> {
        let (raw, fns) = self.pdev(pdev)?;
        let head = lookup(ST_QUEUE_FAMILY_PROPERTIES2).ok_or(ERR_UNKNOWN)?;
        if count == 0 {
            let mut n = 0u32;
            // SAFETY: `n` jest poprawnym wskaźnikiem wyjściowym; tablica null przy pytaniu o liczbę.
            unsafe { (fns.get_queue_family_properties2)(raw, &mut n, std::ptr::null_mut()) };
            return Ok(n.to_le_bytes().to_vec());
        }
        if count > MAX_ARRAY {
            return Err(ERR_UNKNOWN);
        }
        let size = head.size as usize;
        let mut chains = Vec::with_capacity(count as usize);
        for _ in 0..count {
            chains.push(build_chain(chain).map_err(|_| VK_ERROR_FEATURE_NOT_PRESENT)?);
        }
        let mut arr = Words::zeroed(size * count as usize);
        for (i, cb) in chains.iter_mut().enumerate() {
            write_u32(arr.bytes_mut(), i * size, ST_QUEUE_FAMILY_PROPERTIES2);
            write_u64(arr.bytes_mut(), i * size + PNEXT_OFFSET, cb.head_ptr() as usize as u64);
        }
        let mut n = count;
        let p = arr.ptr();
        // SAFETY: tablica ma miejsce na `count` struktur, każda z własnym łańcuchem żyjącym do końca wywołania.
        unsafe { (fns.get_queue_family_properties2)(raw, &mut n, p) };
        let n = n.min(count) as usize;
        let mut out = Vec::new();
        for (i, cb) in chains.into_iter().enumerate().take(n) {
            let mut block = arr.bytes().get(i * size..(i + 1) * size).ok_or(ERR_UNKNOWN)?.to_vec();
            write_u64(&mut block, PNEXT_OFFSET, 0);
            out.extend(block);
            out.extend(cb.into_response_bytes());
        }
        Ok(out)
    }
}

/// Wersja API instancji serwera: zażądana przez klienta albo wersja loadera, jeśli jest wyższa (patrz `create_instance`).
fn effective_api_version(requested: u32, loader: u32) -> u32 {
    requested.max(loader)
}

impl GpuBackend for VulkanBackend {
    fn instance_version(&mut self) -> Result<u32, i32> {
        let _g = GLOBAL_CALLS.lock().map_err(|_| VK_ERROR_INITIALIZATION_FAILED)?;
        let mut v = 0u32;
        // SAFETY: `v` jest poprawnym wskaźnikiem wyjściowym.
        vk_ok(unsafe { (self.global.enumerate_instance_version)(&mut v) })?;
        Ok(v)
    }

    fn instance_extensions(&mut self) -> Result<Vec<(String, u32)>, i32> {
        self.list_instance_extensions()
    }

    fn create_instance(&mut self, api_version: u32, extensions: &[String]) -> Result<u64, i32> {
        // Białą listą jest to, co instancja serwera naprawdę wspiera: nieznane rozszerzenie = błąd,
        // a warstwy (`ppEnabledLayerNames`) nie są nigdy przekazywane.
        let supported = self.list_instance_extensions()?;
        for e in extensions {
            if !supported.iter().any(|(n, _)| n == e) {
                return Err(VK_ERROR_EXTENSION_NOT_PRESENT);
            }
        }
        let names: Vec<CString> = extensions.iter().map(|e| CString::new(e.as_str()).map_err(|_| VK_ERROR_EXTENSION_NOT_PRESENT)).collect::<Result<_, _>>()?;
        let ptrs: Vec<*const c_char> = names.iter().map(|c| c.as_ptr()).collect();

        let mut app = Words::zeroed(48);
        write_u32(app.bytes_mut(), 0, ST_APPLICATION_INFO);
        // Funkcje core nowsze niż wersja instancji nie trafiają do tablicy dyspozytora loadera (wywołanie = skok pod adres 0), a ICD
        // ogłasza aplikacjom 1.3: instancja serwera dostaje więc wersję loadera, jeśli jest wyższa niż zażądana.
        let loader_version = self.instance_version().unwrap_or(0);
        write_u32(app.bytes_mut(), 44, effective_api_version(api_version, loader_version));
        let mut info = Words::zeroed(64);
        write_u32(info.bytes_mut(), 0, ST_INSTANCE_CREATE_INFO);
        write_u64(info.bytes_mut(), 24, app.ptr() as usize as u64);
        write_u32(info.bytes_mut(), 48, ptrs.len() as u32);
        write_u64(info.bytes_mut(), 56, ptrs.as_ptr() as usize as u64);

        let mut raw: VkInstance = std::ptr::null_mut();
        {
            let _g = GLOBAL_CALLS.lock().map_err(|_| VK_ERROR_INITIALIZATION_FAILED)?;
            // SAFETY: struktury i tablica nazw żyją do końca wywołania; układ zgodny z VkInstanceCreateInfo (64 B).
            vk_ok(unsafe { (self.global.create_instance)(info.ptr() as *const c_void, std::ptr::null(), &mut raw) })?;
        }
        // SAFETY: `raw` pochodzi z udanego `vkCreateInstance` tego samego loadera.
        let fns = match unsafe { InstanceFns::load(&self.lib, raw) } {
            Ok(f) => f,
            Err(_) => return Err(VK_ERROR_INITIALIZATION_FAILED),
        };
        match self.instances.insert(InstanceEntry { raw, fns, pdevs: Vec::new() }) {
            Ok(h) => {
                self.replay.set_instance(raw);
                let _ = self.replay.registry.register(h.raw(), Entry { kind: "VkInstance", real: raw as u64, parent: 0, size: 0 });
                Ok(h.raw())
            }
            Err(_) => {
                // SAFETY: instancja została dopiero co utworzona i nie ma jeszcze żadnych obiektów potomnych.
                unsafe { (fns.destroy_instance)(raw, std::ptr::null()) };
                Err(ERR_UNKNOWN)
            }
        }
    }

    fn destroy_instance(&mut self, instance: u64) -> Result<(), i32> {
        let entry = self.instances.remove(Handle(instance)).ok_or(ERR_UNKNOWN)?;
        self.replay.destroy_devices();
        self.replay.registry.unregister(instance);
        for p in &entry.pdevs {
            self.pdevs.remove(*p);
            self.replay.registry.unregister(p.raw());
        }
        // SAFETY: instancja jest ważna i wyjęta z tablicy, więc nikt jej już nie użyje.
        unsafe { (entry.fns.destroy_instance)(entry.raw, std::ptr::null()) };
        Ok(())
    }

    fn enum_physical_devices(&mut self, instance: u64) -> Result<Vec<u64>, i32> {
        let (raw, fns, known) = {
            let e = self.instances.get(Handle(instance)).ok_or(ERR_UNKNOWN)?;
            (e.raw, e.fns, e.pdevs.clone())
        };
        if !known.is_empty() {
            return Ok(known.iter().map(|h| h.raw()).collect());
        }
        let mut n = 0u32;
        // SAFETY: wskaźniki wyjściowe są poprawne, instancja jest ważna.
        vk_ok(unsafe { (fns.enumerate_physical_devices)(raw, &mut n, std::ptr::null_mut()) })?;
        if n > MAX_ARRAY {
            return Err(ERR_UNKNOWN);
        }
        let mut v: Vec<VkPhysicalDevice> = vec![std::ptr::null_mut(); n as usize];
        vk_ok(unsafe { (fns.enumerate_physical_devices)(raw, &mut n, v.as_mut_ptr()) })?;
        let mut handles = Vec::new();
        for p in v.into_iter().take(n as usize) {
            let h = self.pdevs.insert(PdevEntry { raw: p, instance: Handle(instance) }).map_err(|_| ERR_UNKNOWN)?;
            let _ = self.replay.registry.register(h.raw(), Entry { kind: "VkPhysicalDevice", real: p as u64, parent: 0, size: 0 });
            handles.push(h);
        }
        if let Some(e) = self.instances.get_mut(Handle(instance)) {
            e.pdevs = handles.clone();
        }
        // Diagnostyka: lista urządzeń widziana przez ten proces przy każdej instancji (zdarzało się, że znikała karta NVIDIA).
        let mut names = Vec::new();
        for p in &handles {
            if let Ok(d) = self.query_head(p.raw(), ST_PROPERTIES2, 0, &[]) {
                let name = d.get(36..36 + 256).map(|n| String::from_utf8_lossy(n.split(|&b| b == 0).next().unwrap_or(&[])).into_owned()).unwrap_or_default();
                names.push(name);
            }
        }
        eprintln!("gpu-renderd: instancja {instance:#x} widzi {} urządzeń: {names:?}", handles.len());
        Ok(handles.iter().map(|h| h.raw()).collect())
    }

    fn get_physical_device(&mut self, pdev: u64, func: PdFunc, arg: u32, count: u32, chain: &[u32]) -> Result<Vec<u8>, i32> {
        match func {
            PdFunc::Properties2 => self.query_head(pdev, ST_PROPERTIES2, 0, chain),
            PdFunc::Features2 => self.query_head(pdev, ST_FEATURES2, 0, chain),
            PdFunc::MemoryProperties2 => self.query_head(pdev, ST_MEMORY_PROPERTIES2, 0, chain),
            PdFunc::FormatProperties2 => self.query_head(pdev, ST_FORMAT_PROPERTIES2, arg, chain),
            PdFunc::QueueFamilyProperties2 => self.query_queue_families(pdev, count, chain),
            // Warianty bez `2` składa ICD z odpowiedzi `*2`.
            _ => Err(ERR_UNKNOWN),
        }
    }

    fn execute(&mut self, batch: &[u8]) -> Result<(), i32> {
        self.replay.execute(batch)
    }

    fn query(&mut self, data: &[u8]) -> Result<Vec<u8>, i32> {
        self.replay.query(data)
    }

    fn mem_write(&mut self, memory: u64, offset: u64, data: &[u8]) -> Result<(), i32> {
        self.replay.mem_write(memory, offset, data)
    }

    fn mem_read(&mut self, memory: u64, offset: u64, len: u32) -> Result<Vec<u8>, i32> {
        self.replay.mem_read(memory, offset, len)
    }

    fn mem_prefetch(&mut self, items: &[DeltaItem]) {
        self.replay.mem_prefetch(items);
    }

    fn mem_prefetch_end(&mut self) {
        self.replay.mem_prefetch_end();
    }

    fn mem_read_delta(&mut self, memory: u64, max_bytes: usize) -> Result<Vec<u8>, i32> {
        self.replay.mem_read_delta(memory, max_bytes)
    }

    fn mem_read_delta_ranges(&mut self, memory: u64, ranges: &[(u64, u64)], max_bytes: usize, cached: bool) -> Result<Vec<u8>, i32> {
        self.replay.mem_read_delta_ranges(memory, ranges, max_bytes, cached)
    }

    fn device_extensions(&mut self, pdev: u64) -> Result<Vec<(String, u32)>, i32> {
        let (raw, fns) = self.pdev(pdev)?;
        let mut n = 0u32;
        // SAFETY: wskaźniki wyjściowe są poprawne, urządzenie należy do żywej instancji (sprawdzone w `pdev`).
        vk_ok(unsafe { (fns.enumerate_device_extension_properties)(raw, std::ptr::null(), &mut n, std::ptr::null_mut()) })?;
        let mut v = vec![VkExtensionProperties { extension_name: [0; 256], spec_version: 0 }; n as usize];
        vk_ok(unsafe { (fns.enumerate_device_extension_properties)(raw, std::ptr::null(), &mut n, v.as_mut_ptr()) })?;
        Ok(v.iter().take(n as usize).filter_map(|e| ext_name(e).map(|s| (s, e.spec_version))).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpu_proto::stype::STYPE_TABLE;

    fn backend() -> Option<VulkanBackend> {
        match VulkanBackend::new() {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("POMINIĘTO: brak libvulkan ({e})");
                None
            }
        }
    }

    fn st(name: &str) -> u32 {
        STYPE_TABLE.iter().find(|e| e.name == name).unwrap().stype
    }

    fn u32_at(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }

    fn u64_at(b: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }

    /// Instancja i pierwsza fizyczna karta.
    fn setup(b: &mut VulkanBackend) -> (u64, u64) {
        let i = b.create_instance(0x0040_1000, &[]).unwrap();
        let pds = b.enum_physical_devices(i).unwrap();
        assert!(!pds.is_empty());
        (i, pds[0])
    }

    #[test]
    fn version_and_extensions_are_reported() {
        let Some(mut b) = backend() else { return };
        assert!(b.instance_version().unwrap() >= 0x0040_1000);
        assert!(b.instance_extensions().unwrap().iter().any(|(n, _)| n == "VK_KHR_surface"));
    }

    #[test]
    fn handles_are_opaque_stable_and_revoked_on_destroy() {
        let Some(mut b) = backend() else { return };
        let (i, p) = setup(&mut b);
        assert_ne!(i >> 32, 0, "uchwyt ma generację, nie jest adresem");
        assert_eq!(b.enum_physical_devices(i).unwrap()[0], p, "powtórne wyliczenie daje te same uchwyty");
        assert!(b.get_physical_device(p, PdFunc::Properties2, 0, 0, &[]).is_ok());
        b.destroy_instance(i).unwrap();
        assert!(b.get_physical_device(p, PdFunc::Properties2, 0, 0, &[]).is_err(), "karta unieważniona");
        assert!(b.destroy_instance(i).is_err(), "podwójne zniszczenie");
        assert!(b.enum_physical_devices(i).is_err());
    }

    #[test]
    fn properties2_returns_head_then_chain_with_zeroed_pnext() {
        let Some(mut b) = backend() else { return };
        let (_, p) = setup(&mut b);
        let v12 = st("VkPhysicalDeviceVulkan12Properties");
        let out = b.get_physical_device(p, PdFunc::Properties2, 0, 0, &[v12]).unwrap();
        let head = 840;
        assert_eq!(u32_at(&out, 0), 1_000_059_001);
        assert_eq!(u64_at(&out, 8), 0, "pNext głowy zerowy");
        assert!(u32_at(&out, 16) >= 0x0040_0000, "apiVersion");
        let name = CStr::from_bytes_until_nul(&out[36..36 + 256]).unwrap().to_string_lossy();
        assert!(!name.is_empty());
        assert_eq!(u32_at(&out, head), v12, "łańcuch zaczyna się zaraz po głowie");
        assert_eq!(u64_at(&out, head + 8), 0, "pNext elementu łańcucha zerowy");
        assert_eq!(out.len(), head + lookup(v12).unwrap().size as usize, "bez wypełnienia na końcu");
    }

    #[test]
    fn features_and_memory_properties_have_expected_shapes() {
        let Some(mut b) = backend() else { return };
        let (_, p) = setup(&mut b);
        let f = b.get_physical_device(p, PdFunc::Features2, 0, 0, &[]).unwrap();
        assert_eq!((f.len(), u32_at(&f, 0)), (240, 1_000_059_000));
        let m = b.get_physical_device(p, PdFunc::MemoryProperties2, 0, 0, &[]).unwrap();
        assert_eq!((m.len(), u32_at(&m, 0)), (536, 1_000_059_006));
        assert!(u32_at(&m, 16) > 0, "memoryTypeCount");
        let fp = b.get_physical_device(p, PdFunc::FormatProperties2, 37, 0, &[]).unwrap();
        assert_eq!((fp.len(), u32_at(&fp, 0)), (32, 1_000_059_002));
        assert!(u32_at(&fp, 16) != 0 || u32_at(&fp, 20) != 0 || u32_at(&fp, 24) != 0, "VK_FORMAT_R8G8B8A8_UNORM ma jakieś cechy");
    }

    #[test]
    fn queue_families_use_count_then_blocks() {
        let Some(mut b) = backend() else { return };
        let (_, p) = setup(&mut b);
        let c = b.get_physical_device(p, PdFunc::QueueFamilyProperties2, 0, 0, &[]).unwrap();
        assert_eq!(c.len(), 4);
        let n = u32_at(&c, 0) as usize;
        assert!(n >= 1);
        let d = b.get_physical_device(p, PdFunc::QueueFamilyProperties2, 0, n as u32, &[]).unwrap();
        assert_eq!(d.len(), n * 40);
        assert_eq!(u32_at(&d, 0), 1_000_059_005);
        assert!(u32_at(&d, 16) != 0, "queueFlags pierwszej rodziny");
        let more = b.get_physical_device(p, PdFunc::QueueFamilyProperties2, 0, n as u32 + 5, &[]).unwrap();
        assert_eq!(more.len(), n * 40, "prosimy o więcej niż jest: dostajemy tyle, ile jest");
    }

    #[test]
    fn device_extensions_are_listed() {
        let Some(mut b) = backend() else { return };
        let (_, p) = setup(&mut b);
        assert!(!b.device_extensions(p).unwrap().is_empty());
    }

    #[test]
    fn hostile_input_is_rejected_cleanly() {
        let Some(mut b) = backend() else { return };
        let (i, p) = setup(&mut b);
        let v12 = st("VkPhysicalDeviceVulkan12Properties");
        assert_eq!(b.get_physical_device(p, PdFunc::Properties2, 0, 0, &[v12, v12]), Err(VK_ERROR_FEATURE_NOT_PRESENT));
        assert_eq!(b.get_physical_device(p, PdFunc::Properties2, 0, 0, &[123_456_789]), Err(VK_ERROR_FEATURE_NOT_PRESENT));
        assert_eq!(b.get_physical_device(p, PdFunc::Properties2, 0, 0, &[st("VkInstanceCreateInfo")]), Err(VK_ERROR_FEATURE_NOT_PRESENT));
        assert_eq!(b.get_physical_device(p, PdFunc::Properties, 0, 0, &[]), Err(ERR_UNKNOWN), "warianty bez *2 nieobsługiwane");
        assert_eq!(b.get_physical_device(0xdead_beef, PdFunc::Properties2, 0, 0, &[]), Err(ERR_UNKNOWN));
        assert_eq!(b.get_physical_device(p, PdFunc::QueueFamilyProperties2, 0, MAX_ARRAY + 1, &[]), Err(ERR_UNKNOWN));
        assert_eq!(b.create_instance(1, &["VK_NIE_MA_TAKIEGO".to_string()]), Err(VK_ERROR_EXTENSION_NOT_PRESENT));
        assert_eq!(b.create_instance(1, &["a\0b".to_string()]), Err(VK_ERROR_EXTENSION_NOT_PRESENT));
        assert!(b.destroy_instance(0).is_err());
        assert!(b.get_physical_device(i, PdFunc::Properties2, 0, 0, &[]).is_err(), "uchwyt instancji nie jest kartą");
    }

    #[test]
    fn instance_limit_is_enforced() {
        let Some(mut b) = backend() else { return };
        let mut hs = Vec::new();
        for _ in 0..MAX_INSTANCES {
            hs.push(b.create_instance(0x0040_1000, &[]).unwrap());
        }
        assert_eq!(b.create_instance(0x0040_1000, &[]), Err(ERR_UNKNOWN));
        for h in hs {
            b.destroy_instance(h).unwrap();
        }
        assert!(b.create_instance(0x0040_1000, &[]).is_ok());
    }

    #[test]
    fn sessions_end_to_end_through_the_handler_on_real_vulkan() {
        use crate::handler::Session;
        use gpu_proto::msg::{Request, Response, PROTOCOL_VERSION};
        let Some(b) = backend() else { return };
        let mut s = Session::new(b, b"t".to_vec());
        assert!(matches!(s.handle(Request::Hello { version: PROTOCOL_VERSION, token: b"t".to_vec() }), Response::HelloOk { .. }));
        let Response::Handle(i) = s.handle(Request::CreateInstance { api_version: 0x0040_1000, extensions: vec![] }) else { panic!() };
        let Response::Handles(p) = s.handle(Request::EnumPhysicalDevices { instance: i }) else { panic!() };
        let r = s.handle(Request::GetPhysicalDevice { pdev: p[0], func: PdFunc::Properties2, arg: 0, count: 0, chain: vec![] });
        assert!(matches!(&r, Response::Data(d) if d.len() == 840), "{r:?}");
        s.cleanup();
    }
    #[test]
    fn server_instance_gets_at_least_the_loader_version() {
        assert_eq!(effective_api_version(0x0040_1000, 0x0040_4170), 0x0040_4170, "klient prosi o 1.1, loader umie 1.4");
        assert_eq!(effective_api_version(0x0040_3000, 0x0040_2000), 0x0040_3000, "zażądana wyższa zostaje");
        assert_eq!(effective_api_version(0x0040_1000, 0), 0x0040_1000, "bez wersji loadera zostaje zażądana");
    }

}
