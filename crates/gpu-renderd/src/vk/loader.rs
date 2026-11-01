//! Ładowanie `libvulkan.so.1` przez `dlopen` (bez `ash`) i tabele wskaźników na funkcje M1.
//! Wskaźniki pobieramy przez `vkGetInstanceProcAddr`: globalne z instancją null, resztę per instancja.
//! Brak symbolu to błąd (`LoadError::MissingSymbol`), nigdy panika.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

const RTLD_NOW: c_int = 2;
const RTLD_LOCAL: c_int = 0;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

/// Blokada na wywołania GLOBALNE loadera (`vkCreateInstance`, `vkEnumerateInstance*`): loader Vulkana 1.4
/// potrafi się wywrócić (SIGSEGV w `vkCreateInstance`), gdy dwa wątki naraz inicjalizują jego stan.
/// Backend musi trzymać tę blokadę na czas takiego wywołania; testy robią to samo.
pub static GLOBAL_CALLS: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub type VkResult = i32;
pub type VkInstance = *mut c_void;
pub type VkPhysicalDevice = *mut c_void;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// Żadna z kandydujących bibliotek się nie załadowała (treść `dlerror` ostatniej próby).
    NotFound(String),
    /// Biblioteka nie eksportuje `vkGetInstanceProcAddr`.
    NoEntryPoint,
    /// `vkGetInstanceProcAddr` zwrócił null dla wymaganej funkcji.
    MissingSymbol(&'static str),
}

type Gipa = unsafe extern "C" fn(VkInstance, *const c_char) -> *const c_void;

pub struct LibVulkan {
    gipa: Gipa,
}

// SAFETY: wskaźnik funkcji jest tylko do odczytu po utworzeniu;
// `vkGetInstanceProcAddr` jest wątkowo-bezpieczny wg specyfikacji.
unsafe impl Send for LibVulkan {}
unsafe impl Sync for LibVulkan {}

fn last_dlerror() -> String {
    // SAFETY: `dlerror` zwraca null albo poprawny napis C ważny do następnego wywołania dl*.
    unsafe {
        let p = dlerror();
        if p.is_null() {
            "nieznany błąd dlopen".to_string()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

impl LibVulkan {
    /// Kolejność: zmienna `GPU_RENDERD_VULKAN_LIB` (testy), `libvulkan.so.1`, `libvulkan.so`.
    pub fn load() -> Result<LibVulkan, LoadError> {
        let mut last = LoadError::NotFound("brak kandydatów".into());
        let env = std::env::var("GPU_RENDERD_VULKAN_LIB").ok();
        for name in env.iter().map(String::as_str).chain(["libvulkan.so.1", "libvulkan.so"]) {
            match Self::load_from(name) {
                Ok(l) => return Ok(l),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    pub fn load_from(name: &str) -> Result<LibVulkan, LoadError> {
        let c = CString::new(name).map_err(|_| LoadError::NotFound("nazwa z bajtem NUL".into()))?;
        // SAFETY: `c` jest poprawnym napisem C; flagi są stałymi glibc.
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            return Err(LoadError::NotFound(last_dlerror()));
        }
        // SAFETY: `handle` jest ważny; symbol to wskaźnik na funkcję o sygnaturze z specyfikacji Vulkana.
        let sym = unsafe { dlsym(handle, c"vkGetInstanceProcAddr".as_ptr()) };
        if sym.is_null() {
            unsafe { dlclose(handle) };
            return Err(LoadError::NoEntryPoint);
        }
        let gipa: Gipa = unsafe { std::mem::transmute::<*mut c_void, Gipa>(sym) };
        Ok(LibVulkan { gipa })
    }

    /// Surowy adres funkcji albo `None`, gdy loader jej nie zna.
    ///
    /// # Safety
    /// `instance` musi być null albo ważnym, jeszcze niezniszczonym uchwytem instancji.
    pub unsafe fn proc_addr(&self, instance: VkInstance, name: &CStr) -> Option<*const c_void> {
        // SAFETY: `name` jest napisem C; instancja jest null albo ważnym uchwytem z `vkCreateInstance`.
        let p = (self.gipa)(instance, name.as_ptr());
        (!p.is_null()).then_some(p)
    }
}

// Celowo BEZ `Drop` z `dlclose`: rozładowanie `libvulkan` rozładowuje także sterowniki (Mesa, NVIDIA),
// co kończy się SIGSEGV, gdy inny wątek jeszcze z nich korzysta (zmierzone w testach równoległych).
// Biblioteka żyje do końca procesu, jak w każdym loaderze Vulkana. Instancje niszczy backend.

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkExtensionProperties {
    pub extension_name: [u8; 256],
    pub spec_version: u32,
}

type FnEnumInstanceVersion = unsafe extern "C" fn(*mut u32) -> VkResult;
type FnEnumInstanceExt = unsafe extern "C" fn(*const c_char, *mut u32, *mut VkExtensionProperties) -> VkResult;
type FnCreateInstance = unsafe extern "C" fn(*const c_void, *const c_void, *mut VkInstance) -> VkResult;
type FnDestroyInstance = unsafe extern "C" fn(VkInstance, *const c_void);
type FnEnumPhysical = unsafe extern "C" fn(VkInstance, *mut u32, *mut VkPhysicalDevice) -> VkResult;
type FnEnumDeviceExt = unsafe extern "C" fn(VkPhysicalDevice, *const c_char, *mut u32, *mut VkExtensionProperties) -> VkResult;
type FnGetPdev2 = unsafe extern "C" fn(VkPhysicalDevice, *mut c_void);
type FnGetPdevFormat2 = unsafe extern "C" fn(VkPhysicalDevice, u32, *mut c_void);
type FnGetQueueFamilies2 = unsafe extern "C" fn(VkPhysicalDevice, *mut u32, *mut c_void);

/// Funkcje globalne (instancja null).
#[derive(Clone, Copy)]
pub struct GlobalFns {
    pub enumerate_instance_version: FnEnumInstanceVersion,
    pub enumerate_instance_extension_properties: FnEnumInstanceExt,
    pub create_instance: FnCreateInstance,
}

/// Funkcje instancyjne M1.
#[derive(Clone, Copy)]
pub struct InstanceFns {
    pub destroy_instance: FnDestroyInstance,
    pub enumerate_physical_devices: FnEnumPhysical,
    pub enumerate_device_extension_properties: FnEnumDeviceExt,
    pub get_properties2: FnGetPdev2,
    pub get_features2: FnGetPdev2,
    pub get_memory_properties2: FnGetPdev2,
    pub get_format_properties2: FnGetPdevFormat2,
    pub get_queue_family_properties2: FnGetQueueFamilies2,
}

macro_rules! load_fn {
    ($lib:expr, $inst:expr, $name:literal, $ty:ty) => {{
        let cname = CStr::from_bytes_with_nul($name.as_bytes()).map_err(|_| LoadError::MissingSymbol($name))?;
        // SAFETY: `$inst` to null albo ważna instancja przekazana przez wołającego `load`.
        let p = unsafe { $lib.proc_addr($inst, cname) }.ok_or(LoadError::MissingSymbol($name))?;
        // SAFETY: wskaźnik zwrócił loader dla tej nazwy, a typ odpowiada sygnaturze ze specyfikacji Vulkana.
        unsafe { std::mem::transmute::<*const c_void, $ty>(p) }
    }};
}

impl GlobalFns {
    /// `vkEnumerateInstanceVersion` wymaga loadera >= 1.1; stary loader zwraca null i dostajemy błąd.
    pub fn load(lib: &LibVulkan) -> Result<GlobalFns, LoadError> {
        let null = std::ptr::null_mut();
        Ok(GlobalFns {
            enumerate_instance_version: load_fn!(lib, null, "vkEnumerateInstanceVersion\0", FnEnumInstanceVersion),
            enumerate_instance_extension_properties: load_fn!(lib, null, "vkEnumerateInstanceExtensionProperties\0", FnEnumInstanceExt),
            create_instance: load_fn!(lib, null, "vkCreateInstance\0", FnCreateInstance),
        })
    }
}

impl InstanceFns {
    /// # Safety
    /// `instance` musi być ważnym uchwytem instancji utworzonej przez ten sam loader.
    pub unsafe fn load(lib: &LibVulkan, instance: VkInstance) -> Result<InstanceFns, LoadError> {
        Ok(InstanceFns {
            destroy_instance: load_fn!(lib, instance, "vkDestroyInstance\0", FnDestroyInstance),
            enumerate_physical_devices: load_fn!(lib, instance, "vkEnumeratePhysicalDevices\0", FnEnumPhysical),
            enumerate_device_extension_properties: load_fn!(lib, instance, "vkEnumerateDeviceExtensionProperties\0", FnEnumDeviceExt),
            get_properties2: load_fn!(lib, instance, "vkGetPhysicalDeviceProperties2\0", FnGetPdev2),
            get_features2: load_fn!(lib, instance, "vkGetPhysicalDeviceFeatures2\0", FnGetPdev2),
            get_memory_properties2: load_fn!(lib, instance, "vkGetPhysicalDeviceMemoryProperties2\0", FnGetPdev2),
            get_format_properties2: load_fn!(lib, instance, "vkGetPhysicalDeviceFormatProperties2\0", FnGetPdevFormat2),
            get_queue_family_properties2: load_fn!(lib, instance, "vkGetPhysicalDeviceQueueFamilyProperties2\0", FnGetQueueFamilies2),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Option<LibVulkan> {
        match LibVulkan::load() {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("POMINIĘTO: brak libvulkan w środowisku testowym ({e:?})");
                None
            }
        }
    }

    #[test]
    fn missing_library_is_a_not_found_error() {
        assert!(matches!(LibVulkan::load_from("libnie-istnieje-xyz.so.9"), Err(LoadError::NotFound(_))));
        assert!(matches!(LibVulkan::load_from("a\0b"), Err(LoadError::NotFound(_))));
    }

    #[test]
    fn library_without_entry_point_is_rejected() {
        // libm istnieje wszędzie, ale nie ma vkGetInstanceProcAddr.
        assert!(matches!(LibVulkan::load_from("libm.so.6"), Err(LoadError::NoEntryPoint)));
    }

    #[test]
    fn unknown_function_names_give_none_not_a_crash() {
        let Some(l) = lib() else { return };
        assert!(unsafe { l.proc_addr(std::ptr::null_mut(), c"vkNieMaTakiejFunkcji") }.is_none());
    }

    #[test]
    fn global_functions_report_version_at_least_1_1() {
        let Some(l) = lib() else { return };
        let _g = GLOBAL_CALLS.lock().unwrap();
        let g = GlobalFns::load(&l).unwrap();
        let mut v = 0u32;
        // SAFETY: `v` jest poprawnym wskaźnikiem wyjściowym.
        assert_eq!(unsafe { (g.enumerate_instance_version)(&mut v) }, 0);
        assert!(v >= 0x0040_1000, "wersja instancji {v:#x}");
    }

    #[test]
    fn instance_extensions_use_the_two_call_idiom() {
        let Some(l) = lib() else { return };
        let _g = GLOBAL_CALLS.lock().unwrap();
        let g = GlobalFns::load(&l).unwrap();
        let mut n = 0u32;
        assert_eq!(unsafe { (g.enumerate_instance_extension_properties)(std::ptr::null(), &mut n, std::ptr::null_mut()) }, 0);
        assert!(n > 0);
        let mut v = vec![VkExtensionProperties { extension_name: [0; 256], spec_version: 0 }; n as usize];
        assert_eq!(unsafe { (g.enumerate_instance_extension_properties)(std::ptr::null(), &mut n, v.as_mut_ptr()) }, 0);
        let names: Vec<String> = v.iter().take(n as usize).map(|e| CStr::from_bytes_until_nul(&e.extension_name).unwrap().to_string_lossy().into_owned()).collect();
        assert!(names.iter().any(|x| x == "VK_KHR_surface"), "{names:?}");
    }

    #[test]
    fn create_enumerate_and_destroy_a_real_instance() {
        let Some(l) = lib() else { return };
        let _g = GLOBAL_CALLS.lock().unwrap();
        let g = GlobalFns::load(&l).unwrap();
        // VkInstanceCreateInfo (64 B): sType=1, reszta zerowa, bez rozszerzeń i warstw.
        let mut info = [0u64; 8];
        info[0] = 1;
        let mut inst: VkInstance = std::ptr::null_mut();
        assert_eq!(unsafe { (g.create_instance)(info.as_ptr() as *const c_void, std::ptr::null(), &mut inst) }, 0);
        assert!(!inst.is_null());
        let f = unsafe { InstanceFns::load(&l, inst) }.unwrap();
        let mut n = 0u32;
        assert_eq!(unsafe { (f.enumerate_physical_devices)(inst, &mut n, std::ptr::null_mut()) }, 0);
        assert!(n >= 1, "brak urządzeń Vulkan");
        let mut pd = vec![std::ptr::null_mut(); n as usize];
        assert_eq!(unsafe { (f.enumerate_physical_devices)(inst, &mut n, pd.as_mut_ptr()) }, 0);
        assert!(pd.iter().all(|p| !p.is_null()));
        // VkPhysicalDeviceProperties2 (840 B), sType = 1000059001: deviceName leży w offsecie 16+20.
        let mut props = vec![0u64; 105];
        props[0] = 1_000_059_001;
        unsafe { (f.get_properties2)(pd[0], props.as_mut_ptr() as *mut c_void) };
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(props.as_ptr() as *const u8, 840) };
        let name = CStr::from_bytes_until_nul(&bytes[36..36 + 256]).unwrap().to_string_lossy();
        assert!(!name.is_empty(), "pusta nazwa urządzenia");
        unsafe { (f.destroy_instance)(inst, std::ptr::null()) };
    }
}
