//! `gpu-icd`: sterownik ICD Vulkana (biblioteka ładowana przez loader), który zamiast lokalnego GPU
//! używa karty zdalnej przez `gpu-renderd`. M1: wyliczanie kart i zapytania o ich właściwości.
//!
//! Konfiguracja (zmienne środowiskowe): `GPU_ICD_ADDR` (adres:port demona), `GPU_ICD_TOKEN_FILE` albo
//! `GPU_ICD_TOKEN`, opcjonalnie `GPU_ICD_ALL_DEVICES=1` (domyślnie pomijamy urządzenia typu CPU, np. llvmpipe).
//!
//! Obiekty dyspozycyjne (`VkInstance`, `VkPhysicalDevice`) zaczynają się od wskaźnika loadera
//! (interfejs ICD v5), po którym mamy własne pola.

#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::Mutex;

use gpu_proto::msg::{PdFunc, Request, Response};
use gpu_proto::stype::lookup;
use gpu_renderd::client::Client;
use gpu_renderd::handler::decode_extensions;

mod descpool;
mod exts;
mod fwd;
mod local;
mod memreq;
mod memtrack;
mod present;
mod swapchain;
mod templates;
mod descsynth;
mod gpuwrite;
mod latency;
mod video;
mod sbuild;
mod wsi;
mod lazy;
mod lazymap;
mod link;
mod watchdog;
mod shadow;
#[rustfmt::skip]
mod icd_table;

type VkResult = i32;
const VK_SUCCESS: VkResult = 0;
const VK_INCOMPLETE: VkResult = 5;
const VK_ERROR_OUT_OF_HOST_MEMORY: VkResult = -1;
const VK_ERROR_INITIALIZATION_FAILED: VkResult = -3;
const VK_ERROR_EXTENSION_NOT_PRESENT: VkResult = -7;
const VK_ERROR_FORMAT_NOT_SUPPORTED: VkResult = -11;

/// Najwyższa wersja API, którą ogłaszamy (1.3.999): funkcje 1.4 nie są jeszcze przekazywane.
pub const MAX_API_VERSION: u32 = (1 << 22) | (3 << 12) | 999;
const ICD_INTERFACE_VERSION: u32 = 5;
const MAX_CHAIN_WALK: usize = 128;
const PNEXT: usize = 8;
const VK_PHYSICAL_DEVICE_TYPE_CPU: u32 = 4;

/// Rozszerzenia urządzenia, które ogłaszamy: tylko te, których funkcje ICD naprawdę implementuje.
/// Serwer ma ich ~250 (m.in. Vulkan Video), ale ogłoszenie nieobsługiwanego rozszerzenia kończy się
/// wskaźnikami null w loaderze i awarią aplikacji. Lista rośnie wraz z kolejnymi etapami (M2: swapchain itd.).
pub const ADVERTISED_DEVICE_EXTENSIONS: &[&str] = &["VK_KHR_swapchain"];

const ST_FEATURES2: u32 = 1_000_059_000;
const ST_PROPERTIES2: u32 = 1_000_059_001;
const ST_FORMAT2: u32 = 1_000_059_002;
const ST_MEMORY2: u32 = 1_000_059_006;

#[repr(C)]
struct InstanceObj {
    loader_data: usize,
    /// Identyfikator w protokole; zawsze pod offsetem 8 (tłumaczenie uchwytów dyspozycyjnych).
    remote: u64,
    state: Mutex<InstState>,
}

struct InstState {
    client: link::Link,
    remote: u64,
    pdevs: Vec<*mut PdevObj>,
    cache: HashMap<(u64, u8, u32, u32), Vec<u8>>,
    /// Zakodowane polecenia czekające na wysłanie (partia).
    batch: Vec<u8>,
    next_id: u64,
    /// Rozmiary pamięci przydzielonych na serwerze i lokalne kopie (tylko odwzorowane).
    mem_sizes: HashMap<u64, u64>,
    /// Skróty stron wysłanych w sesji (tylko diagnostyka przy `GPU_ICD_DEBUG`).
    seen_pages: std::collections::HashSet<u64>,
    /// Cache wymagań pamięci obrazów i buforów (bez obiegu po sieci dla identycznych obiektów).
    memreq: memreq::MemReqCache,
    /// Księgowość pul i układów deskryptorów (spekulatywne odpowiedzi na `vkAllocateDescriptorSets`).
    desc: descpool::DescriptorAccounts,
    /// Czy pamięć wolno odświeżać z serwera po oczekiwaniu (typ HOST_CACHED albo mała); duże pamięci write-combined czyta się wolno.
    mem_refresh: HashMap<u64, bool>,
    /// Pamięć o pewnie znanym typie HOST_CACHED (nieznany typ = `false`): serwer porównuje ją w miejscu, bez kopii pośredniej.
    mem_cached: HashMap<u64, bool>,
    /// Które zakresy odwzorowanej pamięci GPU może zapisać (decyduje, co odświeżać po oczekiwaniu).
    gpuw: gpuwrite::GpuWrites,
    /// Lokalne wyliczanie deskryptorów buforów (uczy się i weryfikuje na odpowiedziach serwera; patrz `descsynth`).
    synth: descsynth::Synth,
    /// Pamięci, dla których plan odświeżania został już zapisany w logu (jednorazowo, przy `GPU_ICD_STATS=periodic`).
    plans_logged: std::collections::HashSet<u64>,
    /// Od ostatniego odświeżenia pamięci był `vkQueueSubmit`, więc GPU mogło ją zmienić.
    gpu_dirty: bool,
    /// Był `vkQueueSubmit` od ostatniego skanu odwzorowanej pamięci: skan (zapisy na serwer) następuje tuż przed wysłaniem partii, patrz `settle_memory`.
    scan_pending: bool,
    /// Licznik zgłoszeń `vkQueueSubmit`: wątek czekający bez blokady (patrz `fwd::Pending`) kasuje stan `gpu_dirty` tylko wtedy, gdy nikt inny
    /// nie zgłosił pracy w międzyczasie.
    submit_gen: u64,
    mems: HashMap<u64, fwd::Shadow>,
    /// Kolejki już wydane aplikacji: (urządzenie, rodzina, indeks) -> obiekt.
    queues: HashMap<(u64, u32, u32), usize>,
    device_lost: bool,
    stats: fwd::Stats,
    /// Szablony aktualizacji deskryptorów (identyfikator -> wpisy), potrzebne do rozwinięcia `vkUpdateDescriptorSetWithTemplate`.
    templates: HashMap<u64, Vec<templates::Entry>>,
    swapchains: HashMap<u64, swapchain::Swapchain>,
    /// Urządzenie (identyfikator) -> zdalna karta fizyczna (do wyboru typów pamięci).
    device_pdev: HashMap<u64, u64>,
    /// Adres i token demona (do otwarcia połączenia wideo).
    addr: String,
    token: Vec<u8>,
}

/// Lokalny sterownik do prezentacji, otwierany leniwie (raz); `None` gdy go nie ma.
static LOCAL: Mutex<Option<local::LocalVulkan>> = Mutex::new(None);
static LOCAL_TRIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn with_local<R>(f: impl FnOnce(&local::LocalVulkan) -> R) -> Option<R> {
    let mut g = lock(&LOCAL);
    if g.is_none() && !LOCAL_TRIED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        match local::LocalVulkan::open() {
            Ok(l) => *g = Some(l),
            Err(e) => {
                if std::env::var("GPU_ICD_DEBUG").is_ok() {
                    eprintln!("[gpu-icd] lokalny sterownik niedostępny: {e}");
                }
            }
        }
    }
    g.as_ref().map(f)
}

/// Rozszerzenia instancji, które ogłaszamy (powierzchnie okien, o ile ma je lokalny sterownik).
fn instance_extensions() -> Vec<(&'static str, u32)> {
    with_local(|l| wsi::advertised_instance_extensions(&l.enabled_instance_exts)).unwrap_or_else(|| wsi::advertised_instance_extensions(&[]))
}

/// Jedyna instancja procesu (M2a); polecenia generyczne szukają przez nią połączenia.
static CURRENT: std::sync::atomic::AtomicPtr<InstanceObj> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[repr(C)]
struct PdevObj {
    loader_data: usize,
    remote: u64,
    instance: *const InstanceObj,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl InstState {
    /// Jedno zapytanie do demona z pamięcią podręczną (klucz: karta, funkcja, argument, sType łańcucha).
    fn query(&mut self, pdev: u64, func: PdFunc, arg: u32, count: u32, chain: &[u32]) -> Option<Vec<u8>> {
        let key = (pdev, func as u8, arg, chain.first().copied().unwrap_or(0) ^ count.rotate_left(16));
        if let Some(v) = self.cache.get(&key) {
            return Some(v.clone());
        }
        let r = self.client.call(&Request::GetPhysicalDevice { pdev, func, arg, count, chain: chain.to_vec() }).ok()?;
        match r {
            Response::Data(d) => {
                self.cache.insert(key, d.clone());
                Some(d)
            }
            _ => None,
        }
    }
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4).and_then(|s| s.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0)
}

fn config_token() -> Option<Vec<u8>> {
    if let Ok(t) = std::env::var("GPU_ICD_TOKEN") {
        return Some(t.trim().as_bytes().to_vec());
    }
    let f = std::env::var("GPU_ICD_TOKEN_FILE").ok()?;
    Some(std::fs::read_to_string(f).ok()?.trim().as_bytes().to_vec())
}

/// Kopiuje ciało odpowiedzi (bez nagłówka sType/pNext, 16 B) do struktury klienta o rozmiarze `size`.
unsafe fn fill_body(dst: *mut u8, src: &[u8], size: usize) {
    if size > 16 && src.len() >= size {
        std::ptr::copy_nonoverlapping(src.as_ptr().add(16), dst.add(16), size - 16);
    }
}

/// Struktury łańcucha `pNext` klienta: (adres, sType).
unsafe fn walk_chain(head: *mut u8) -> Vec<(*mut u8, u32)> {
    let mut out = Vec::new();
    let mut p = std::ptr::read_unaligned(head.add(PNEXT) as *const *mut u8);
    while !p.is_null() && out.len() < MAX_CHAIN_WALK {
        let st = std::ptr::read_unaligned(p as *const u32);
        out.push((p, st));
        p = std::ptr::read_unaligned(p.add(PNEXT) as *const *mut u8);
    }
    out
}

/// Wypełnia strukturę główną `head` (typu `head_stype`) i jej łańcuch danymi ze zdalnej karty.
unsafe fn fill2(pd: &PdevObj, func: PdFunc, arg: u32, head: *mut u8, head_stype: u32) {
    let Some(entry) = lookup(head_stype) else { return };
    let inst = &*pd.instance;
    let mut st = crate::fwd::lock_state(inst);
    if let Some(d) = st.query(pd.remote, func, arg, 0, &[]) {
        let mut d = d;
        if let Some(h) = d.get_mut(..entry.size as usize) {
            // Zachowujemy sType i pNext klienta, kopiujemy tylko ciało.
            fill_body(head, h, entry.size as usize);
        }
    }
    for (node, stype) in walk_chain(head) {
        let Some(e) = lookup(stype) else { continue };
        if e.has_output_pointers {
            continue;
        }
        if let Some(d) = st.query(pd.remote, func, arg, 0, &[stype]) {
            if let Some(body) = d.get(entry.size as usize..entry.size as usize + e.size as usize) {
                fill_body(node, body, e.size as usize);
            }
        }
    }
}

unsafe fn pdev<'a>(p: *mut c_void) -> Option<&'a PdevObj> {
    (p as *const PdevObj).as_ref()
}

// ---------- funkcje globalne ----------

unsafe extern "C" fn enumerate_instance_version(p: *mut u32) -> VkResult {
    if p.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    *p = 0x0040_3000; // 1.3.0
    VK_SUCCESS
}

unsafe extern "C" fn enumerate_instance_extension_properties(layer: *const c_char, count: *mut u32, props: *mut u8) -> VkResult {
    if !layer.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let Some(count) = count.as_mut() else { return VK_ERROR_INITIALIZATION_FAILED };
    let list = instance_extensions();
    if std::env::var("GPU_ICD_DEBUG").is_ok() {
        eprintln!("[gpu-icd] vkEnumerateInstanceExtensionProperties: {} rozszerzeń, props {}", list.len(), if props.is_null() { "null" } else { "bufor" });
    }
    if props.is_null() {
        *count = list.len() as u32;
        return VK_SUCCESS;
    }
    let n = (*count as usize).min(list.len());
    for (i, (name, ver)) in list.iter().take(n).enumerate() {
        let dst = props.add(i * 260);
        std::ptr::write_bytes(dst, 0, 260);
        std::ptr::copy_nonoverlapping(name.as_ptr(), dst, name.len().min(255));
        std::ptr::write_unaligned(dst.add(256) as *mut u32, *ver);
    }
    *count = n as u32;
    if n < list.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

/// `GPU_ICD_LOG=<plik>`: przekierowuje stderr procesu do pliku (gry uruchamiane ze Steama nie pokazują terminala).
fn redirect_stderr_to_log() {
    extern "C" {
        fn dup2(old: i32, new: i32) -> i32;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(path) = std::env::var("GPU_ICD_LOG") else { return };
        let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
        use std::os::fd::AsRawFd;
        // SAFETY: deskryptor pliku jest ważny; po dup2 stderr wskazuje na ten sam plik (oryginał zamyka Drop).
        unsafe { dup2(f.as_raw_fd(), 2) };
    });
}

unsafe extern "C" fn create_instance(info: *const u8, _alloc: *const c_void, out: *mut *mut InstanceObj) -> VkResult {
    redirect_stderr_to_log();
    if info.is_null() || out.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    // VkInstanceCreateInfo: enabledExtensionCount @48, ppEnabledExtensionNames @56. Dozwolone tylko ogłoszone.
    let n_ext = std::ptr::read_unaligned(info.add(48) as *const u32) as usize;
    if std::env::var("GPU_ICD_DEBUG").is_ok() {
        eprintln!("[gpu-icd] vkCreateInstance: {n_ext} rozszerzeń w żądaniu");
    }
    if n_ext > 0 {
        let names = std::ptr::read_unaligned(info.add(56) as *const *const *const c_char);
        // Bez tablicy nazw żadne rozszerzenie nie jest poprawne, więc nie ruszamy lokalnego sterownika.
        let allowed = if names.is_null() { Vec::new() } else { instance_extensions() };
        for i in 0..n_ext {
            let p = if names.is_null() { std::ptr::null() } else { *names.add(i) };
            let ok = !p.is_null() && CStr::from_ptr(p).to_str().is_ok_and(|n| allowed.iter().any(|(a, _)| *a == n));
            if !ok {
                if std::env::var("GPU_ICD_DEBUG").is_ok() && !p.is_null() {
                    eprintln!("[gpu-icd] vkCreateInstance: nieogłoszone rozszerzenie {:?}", CStr::from_ptr(p));
                }
                return VK_ERROR_EXTENSION_NOT_PRESENT;
            }
        }
    }
    let (Ok(addr), Some(token)) = (std::env::var("GPU_ICD_ADDR"), config_token()) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Ok(mut client) = Client::connect(addr.as_str(), &token) else { return VK_ERROR_INITIALIZATION_FAILED };
    let Ok(Response::Handle(remote)) = client.call(&Request::CreateInstance { api_version: 0x0040_1000, extensions: vec![] }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Ok(Response::Handles(remote_pds)) = client.call(&Request::EnumPhysicalDevices { instance: remote }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let all = std::env::var("GPU_ICD_ALL_DEVICES").is_ok_and(|v| v == "1");
    let obj = Box::into_raw(Box::new(InstanceObj {
        loader_data: 0,
        remote,
        state: Mutex::new(InstState {
            client: match link::Link::from_client(client) {
                Ok(l) => l,
                Err(_) => return VK_ERROR_INITIALIZATION_FAILED,
            },
            remote,
            pdevs: Vec::new(),
            cache: HashMap::new(),
            batch: Vec::new(),
            next_id: 1 << 20,
            mem_sizes: HashMap::new(),
            desc: {
                let mut d = descpool::DescriptorAccounts::default();
                d.set_margin(fwd::DESC_SPEC_MARGIN);
                d
            },
            seen_pages: Default::default(),
            memreq: Default::default(),
            mem_refresh: HashMap::new(),
            mem_cached: HashMap::new(),
            gpuw: Default::default(),
            synth: Default::default(),
            plans_logged: Default::default(),
            gpu_dirty: false,
            scan_pending: false,
            submit_gen: 0,
            templates: HashMap::new(),
            mems: HashMap::new(),
            queues: HashMap::new(),
            device_lost: false,
            stats: fwd::Stats::default(),
            swapchains: HashMap::new(),
            device_pdev: HashMap::new(),
            addr: addr.clone(),
            token: token.clone(),
        }),
    }));
    fwd::desc_cache_clear();
    fwd::install_lazy();
    watchdog::start(fwd::link_state, fwd::fast_commands);
    CURRENT.store(obj, std::sync::atomic::Ordering::SeqCst);
    {
        let mut st = crate::fwd::lock_state(&*obj);
        for rp in remote_pds {
            if !all {
                let q = st.query(rp, PdFunc::Properties2, 0, 0, &[]);
                if std::env::var("GPU_ICD_DEBUG").is_ok() {
                    eprintln!("[gpu-icd] zdalna karta {rp:#x}: zapytanie {}, typ {:?}", q.is_some(), q.as_ref().map(|d| u32_at(d, 32)));
                }
                let t = q.map(|d| u32_at(&d, 32)).unwrap_or(0);
                if t == VK_PHYSICAL_DEVICE_TYPE_CPU {
                    continue;
                }
            }
            st.pdevs.push(Box::into_raw(Box::new(PdevObj { loader_data: 0, remote: rp, instance: obj })));
        }
    }
    if crate::fwd::lock_state(&*obj).pdevs.is_empty() {
        eprintln!("gpu-icd: UWAGA: serwer {addr} nie udostępnia żadnej karty innej niż CPU (llvmpipe); sprawdź sterownik GPU na serwerze albo ustaw GPU_ICD_ALL_DEVICES=1");
    }
    *out = obj;
    VK_SUCCESS
}

unsafe extern "C" fn destroy_instance(inst: *mut InstanceObj, _alloc: *const c_void) {
    if inst.is_null() {
        return;
    }
    CURRENT.store(std::ptr::null_mut(), std::sync::atomic::Ordering::SeqCst);
    fwd::desc_cache_clear();
    let obj = Box::from_raw(inst);
    let mut st = crate::fwd::lock_state(&obj);
    st.flush();
    fwd::trace_flush();
    if std::env::var("GPU_ICD_STATS").is_ok() {
        eprintln!("{}", st.stats.report());
    }
    let remote = st.remote;
    let _ = st.client.call(&Request::DestroyInstance { instance: remote });
    for p in st.pdevs.drain(..) {
        drop(Box::from_raw(p));
    }
}

// ---------- funkcje instancyjne ----------

unsafe extern "C" fn enumerate_physical_devices(inst: *mut InstanceObj, count: *mut u32, out: *mut *mut PdevObj) -> VkResult {
    let (Some(inst), Some(count)) = (inst.as_ref(), count.as_mut()) else { return VK_ERROR_INITIALIZATION_FAILED };
    let st = crate::fwd::lock_state(inst);
    if std::env::var("GPU_ICD_DEBUG").is_ok() {
        eprintln!("[gpu-icd] vkEnumeratePhysicalDevices: {} urządzeń, żądana liczba {}, bufor {}", st.pdevs.len(), *count, if out.is_null() { "null" } else { "jest" });
    }
    if out.is_null() {
        *count = st.pdevs.len() as u32;
        return VK_SUCCESS;
    }
    let n = (*count as usize).min(st.pdevs.len());
    for (i, p) in st.pdevs.iter().take(n).enumerate() {
        *out.add(i) = *p;
    }
    let incomplete = n < st.pdevs.len();
    *count = n as u32;
    if incomplete {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

/// `apiVersion` karty przycinamy do wersji, której funkcje przekazujemy (1.3.999).
pub fn clamp_api_version(v: u32) -> u32 {
    v.min(MAX_API_VERSION)
}

unsafe fn clamp_at(out: *mut u8) {
    let v = std::ptr::read_unaligned(out as *const u32);
    std::ptr::write_unaligned(out as *mut u32, clamp_api_version(v));
}

unsafe extern "C" fn get_properties2(p: *mut c_void, out: *mut u8) {
    if let (Some(pd), false) = (pdev(p), out.is_null()) {
        fill2(pd, PdFunc::Properties2, 0, out, ST_PROPERTIES2);
        clamp_at(out.add(16)); // VkPhysicalDeviceProperties2.properties.apiVersion
    }
}

unsafe extern "C" fn get_features2(p: *mut c_void, out: *mut u8) {
    if let (Some(pd), false) = (pdev(p), out.is_null()) {
        fill2(pd, PdFunc::Features2, 0, out, ST_FEATURES2);
    }
}

unsafe extern "C" fn get_memory_properties2(p: *mut c_void, out: *mut u8) {
    if let (Some(pd), false) = (pdev(p), out.is_null()) {
        fill2(pd, PdFunc::MemoryProperties2, 0, out, ST_MEMORY2);
    }
}

unsafe extern "C" fn get_format_properties2(p: *mut c_void, format: u32, out: *mut u8) {
    if let (Some(pd), false) = (pdev(p), out.is_null()) {
        fill2(pd, PdFunc::FormatProperties2, format, out, ST_FORMAT2);
    }
}

/// Wersje bez `2`: składamy z odpowiedzi `*2` (ciało struktury zaczyna się 16 B za nagłówkiem).
unsafe fn plain(p: *mut c_void, func: PdFunc, arg: u32, st: u32, out: *mut u8, body_len: usize) {
    let Some(pd) = pdev(p) else { return };
    let Some(e) = lookup(st) else { return };
    if out.is_null() {
        return;
    }
    let mut s = crate::fwd::lock_state(&*pd.instance);
    if let Some(d) = s.query(pd.remote, func, arg, 0, &[]) {
        if let Some(body) = d.get(16..16usize.min(e.size as usize) + body_len) {
            std::ptr::copy_nonoverlapping(body.as_ptr(), out, body.len().min(body_len));
        }
    }
}

unsafe extern "C" fn get_properties(p: *mut c_void, out: *mut u8) {
    plain(p, PdFunc::Properties2, 0, ST_PROPERTIES2, out, 824);
    if !out.is_null() {
        clamp_at(out);
    }
}

unsafe extern "C" fn get_features(p: *mut c_void, out: *mut u8) {
    plain(p, PdFunc::Features2, 0, ST_FEATURES2, out, 220);
}

unsafe extern "C" fn get_memory_properties(p: *mut c_void, out: *mut u8) {
    plain(p, PdFunc::MemoryProperties2, 0, ST_MEMORY2, out, 520);
}

unsafe extern "C" fn get_format_properties(p: *mut c_void, format: u32, out: *mut u8) {
    plain(p, PdFunc::FormatProperties2, format, ST_FORMAT2, out, 12);
}

/// Rodziny kolejek: dwa wywołania (liczba, potem bloki po 40 B dla `*2` albo 24 B dla wersji bez `2`).
unsafe fn queue_families(p: *mut c_void, count: *mut u32, out: *mut u8, stride: usize, v2: bool) {
    let (Some(pd), Some(count)) = (pdev(p), count.as_mut()) else { return };
    let mut s = crate::fwd::lock_state(&*pd.instance);
    let Some(n) = s.query(pd.remote, PdFunc::QueueFamilyProperties2, 0, 0, &[]).map(|d| u32_at(&d, 0)) else { return };
    if out.is_null() {
        *count = n;
        return;
    }
    let want = (*count).min(n);
    let Some(d) = s.query(pd.remote, PdFunc::QueueFamilyProperties2, 0, n, &[]) else { return };
    for i in 0..want as usize {
        let Some(blk) = d.get(i * 40..(i + 1) * 40) else { break };
        let dst = out.add(i * stride);
        if v2 {
            fill_body(dst, blk, 40);
            // Łańcuch klienta (np. globalna priorytetyzacja) pomijamy w M1.
        } else {
            std::ptr::copy_nonoverlapping(blk.as_ptr().add(16), dst, 24);
        }
    }
    *count = want;
}

unsafe extern "C" fn get_queue_family_properties2(p: *mut c_void, count: *mut u32, out: *mut u8) {
    queue_families(p, count, out, 40, true);
}

unsafe extern "C" fn get_queue_family_properties(p: *mut c_void, count: *mut u32, out: *mut u8) {
    queue_families(p, count, out, 24, false);
}

unsafe extern "C" fn enumerate_device_extension_properties(p: *mut c_void, layer: *const c_char, count: *mut u32, out: *mut u8) -> VkResult {
    let (Some(pd), Some(count)) = (pdev(p), count.as_mut()) else { return VK_ERROR_INITIALIZATION_FAILED };
    let list = if layer.is_null() {
        let mut s = crate::fwd::lock_state(&*pd.instance);
        match s.client.call(&Request::EnumDeviceExtensions { pdev: pd.remote }) {
            Ok(Response::Data(d)) => decode_extensions(&d)
                .unwrap_or_default()
                .into_iter()
                .filter(|(n, _)| exts::is_advertisable(n))
                .collect(),
            _ => return VK_ERROR_OUT_OF_HOST_MEMORY,
        }
    } else {
        Vec::new()
    };
    if out.is_null() {
        *count = list.len() as u32;
        return VK_SUCCESS;
    }
    let n = (*count as usize).min(list.len());
    for (i, (name, ver)) in list.iter().take(n).enumerate() {
        let dst = out.add(i * 260);
        std::ptr::write_bytes(dst, 0, 260);
        let b = name.as_bytes();
        std::ptr::copy_nonoverlapping(b.as_ptr(), dst, b.len().min(255));
        std::ptr::write_unaligned(dst.add(256) as *mut u32, *ver);
    }
    *count = n as u32;
    if n < list.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

unsafe extern "C" fn enumerate_device_layer_properties(_p: *mut c_void, count: *mut u32, _out: *mut c_void) -> VkResult {
    if let Some(c) = count.as_mut() {
        *c = 0;
    }
    VK_SUCCESS
}

/// M1: zapytania o formaty obrazów nie są jeszcze przekazywane do serwera (brak w protokole).
unsafe extern "C" fn get_image_format_properties(_p: *mut c_void, _f: u32, _t: u32, _tiling: u32, _usage: u32, _flags: u32, _out: *mut u8) -> VkResult {
    VK_ERROR_FORMAT_NOT_SUPPORTED
}

unsafe extern "C" fn get_sparse_image_format_properties(_p: *mut c_void, _f: u32, _t: u32, _s: u32, _usage: u32, _tiling: u32, count: *mut u32, _out: *mut u8) {
    if let Some(c) = count.as_mut() {
        *c = 0;
    }
}

const VK_TRUE_U32: u32 = 1;

/// `vkCreate*SurfaceKHR`: powierzchnię tworzy lokalny sterownik (Mesa wymaga własnego obiektu), a jego uchwyt oddajemy loaderowi.
unsafe fn make_surface(fn_name: &str, info: *const u8, out: *mut u64) -> VkResult {
    if out.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    match with_local(|l| l.create_surface(fn_name, info as u64)).flatten() {
        Some(h) => {
            *out = h;
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

unsafe extern "C" fn create_wayland_surface(_i: u64, info: *const u8, _a: *const c_void, out: *mut u64) -> VkResult {
    make_surface("vkCreateWaylandSurfaceKHR", info, out)
}

unsafe extern "C" fn create_xcb_surface(_i: u64, info: *const u8, _a: *const c_void, out: *mut u64) -> VkResult {
    make_surface("vkCreateXcbSurfaceKHR", info, out)
}

unsafe extern "C" fn create_xlib_surface(_i: u64, info: *const u8, _a: *const c_void, out: *mut u64) -> VkResult {
    make_surface("vkCreateXlibSurfaceKHR", info, out)
}

unsafe extern "C" fn create_headless_surface(_i: u64, info: *const u8, _a: *const c_void, out: *mut u64) -> VkResult {
    make_surface("vkCreateHeadlessSurfaceEXT", info, out)
}

unsafe extern "C" fn destroy_surface(_i: u64, surface: u64, _a: *const c_void) {
    if surface != 0 {
        with_local(|l| l.destroy_surface(surface));
    }
}

unsafe extern "C" fn wayland_presentation_support(_pd: *mut c_void, _qf: u32, display: u64) -> u32 {
    with_local(|l| l.presentation_support("vkGetPhysicalDeviceWaylandPresentationSupportKHR", 0, display, 0)).unwrap_or(false) as u32
}

unsafe extern "C" fn xcb_presentation_support(_pd: *mut c_void, _qf: u32, connection: u64, visual: u32) -> u32 {
    with_local(|l| l.presentation_support("vkGetPhysicalDeviceXcbPresentationSupportKHR", 0, connection, visual as u64)).unwrap_or(false) as u32
}

unsafe extern "C" fn xlib_presentation_support(_pd: *mut c_void, _qf: u32, dpy: u64, visual: u64) -> u32 {
    with_local(|l| l.presentation_support("vkGetPhysicalDeviceXlibPresentationSupportKHR", 0, dpy, visual)).unwrap_or(false) as u32
}

unsafe extern "C" fn surface_support(_pd: *mut c_void, qf: u32, surface: u64, out: *mut u32) -> VkResult {
    let Some(out) = out.as_mut() else { return VK_ERROR_INITIALIZATION_FAILED };
    if std::env::var("GPU_ICD_DEBUG").is_ok() {
        let w: Vec<u32> = (0..6).map(|i| std::ptr::read_unaligned((surface as *const u32).add(i))).collect();
        eprintln!("[gpu-icd] surface_support: uchwyt {surface:#x}, pierwsze słowa {w:x?}, qf {qf}");
    }
    // Prezentujemy lokalnie, więc o wsparciu decyduje lokalna karta (rodzina graficzna 0); każda rodzina zdalna może rysować.
    match with_local(|l| l.surface_support(0, surface)).flatten() {
        Some(ok) => {
            *out = if ok && qf < 64 { VK_TRUE_U32 } else { 0 };
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

unsafe extern "C" fn surface_capabilities(_pd: *mut c_void, surface: u64, out: *mut u8) -> VkResult {
    if out.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let Some(mut caps) = with_local(|l| l.surface_caps(surface)).flatten() else { return VK_ERROR_INITIALIZATION_FAILED };
    if !wsi::patch_capabilities(&mut caps) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    std::ptr::copy_nonoverlapping(caps.as_ptr(), out, 52);
    VK_SUCCESS
}

unsafe extern "C" fn surface_formats(_pd: *mut c_void, surface: u64, count: *mut u32, out: *mut u8) -> VkResult {
    let Some(count) = count.as_mut() else { return VK_ERROR_INITIALIZATION_FAILED };
    let Some(list) = with_local(|l| l.surface_formats(surface)).flatten().map(|f| wsi::filter_formats(&f)) else { return VK_ERROR_INITIALIZATION_FAILED };
    if out.is_null() {
        *count = list.len() as u32;
        return VK_SUCCESS;
    }
    let n = (*count as usize).min(list.len());
    for (i, (f, cs)) in list.iter().take(n).enumerate() {
        std::ptr::write_unaligned(out.add(i * 8) as *mut u32, *f);
        std::ptr::write_unaligned(out.add(i * 8 + 4) as *mut u32, *cs);
    }
    *count = n as u32;
    if n < list.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

unsafe extern "C" fn surface_present_modes(_pd: *mut c_void, surface: u64, count: *mut u32, out: *mut u32) -> VkResult {
    let Some(count) = count.as_mut() else { return VK_ERROR_INITIALIZATION_FAILED };
    let Some(list) = with_local(|l| l.present_modes(surface)).flatten().map(|m| wsi::filter_present_modes(&m)) else { return VK_ERROR_INITIALIZATION_FAILED };
    if out.is_null() {
        *count = list.len() as u32;
        return VK_SUCCESS;
    }
    let n = (*count as usize).min(list.len());
    for (i, m) in list.iter().take(n).enumerate() {
        std::ptr::write_unaligned(out.add(i), *m);
    }
    *count = n as u32;
    if n < list.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

unsafe extern "C" fn get_device_proc_addr(_d: *mut c_void, name: *const c_char) -> *const c_void {
    vk_icdGetInstanceProcAddr(std::ptr::null_mut(), name)
}

// ---------- punkty wejścia ICD ----------

macro_rules! table {
    ($name:expr, $( $s:literal => $f:expr ),* $(,)?) => {
        match $name {
            $( $s => $f as usize as *const c_void, )*
            _ => std::ptr::null(),
        }
    };
}

/// Adres funkcji o danej nazwie: ręczne (lokalne) albo generyczny punkt wejścia przekazujący do serwera.
/// `vkGetPhysicalDeviceExternal{Buffer,Semaphore,Fence}Properties[KHR]`: zewnętrzne uchwyty (fd, win32) nie istnieją po stronie klienta,
/// więc odpowiadamy lokalnie, że nie są obsługiwane (serwer zgłosiłby typy swojego systemu, których klient nie umie użyć).
/// Wszystkie trzy wyniki mają te same trzy pola `u32` zaraz po `sType`/`pNext` (od offsetu 16): funkcje/cechy, typy do eksportu, typy zgodne.
///
/// # Safety
/// `out` musi wskazywać na poprawną strukturę wynikową jednego z trzech rodzajów (min. 28 bajtów) albo być null.
unsafe extern "C" fn external_properties_none(_pd: *mut c_void, _info: *const u8, out: *mut u8) {
    if !out.is_null() {
        std::ptr::write_bytes(out.add(16), 0, 12);
    }
}

pub fn proc_addr_by_name(name: &str) -> *const c_void {
    let p = proc_addr_lookup(name);
    if p.is_null() {
        if let Ok(path) = std::env::var("GPU_ICD_PROCLOG") {
            // Gra bez sprawdzania null wywołuje taki wskaźnik i pada: lista nazw, o które pytała, a których nie mamy.
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{name}");
            }
        }
    }
    p
}

fn proc_addr_lookup(name: &str) -> *const c_void {
    let p = local_proc_addr(name);
    if !p.is_null() {
        return p;
    }
    match gpu_proto::meta::find_command(exts::resolve_alias(name)) {
        // Funkcje powierzchni, ekranu i swapchaina, których ICD nie implementuje lokalnie, NIE idą generycznie na serwer.
        Some(_) if exts::is_local_only_command(name) => std::ptr::null(),
        Some((op, c)) if c.supported && !LOCAL_ONLY.contains(&name) => icd_table::ENTRIES[op as usize] as usize as *const c_void,
        _ => std::ptr::null(),
    }
}

/// Polecenia, które ICD obsługuje wyłącznie lokalnie (nie przekazuje generycznie).
const LOCAL_ONLY: &[&str] = &["vkCreateWaylandSurfaceKHR", "vkCreateXcbSurfaceKHR", "vkCreateXlibSurfaceKHR", "vkCreateHeadlessSurfaceEXT", "vkDestroySurfaceKHR", "vkGetPhysicalDeviceWaylandPresentationSupportKHR", "vkGetPhysicalDeviceXcbPresentationSupportKHR", "vkGetPhysicalDeviceXlibPresentationSupportKHR", "vkCreateInstance", "vkDestroyInstance", "vkEnumeratePhysicalDevices", "vkEnumerateInstanceVersion", "vkEnumerateInstanceExtensionProperties", "vkEnumerateInstanceLayerProperties"];

fn local_proc_addr(name: &str) -> *const c_void {
    table!(name,
        "vkEnumerateInstanceVersion" => enumerate_instance_version as unsafe extern "C" fn(*mut u32) -> VkResult,
        "vkEnumerateInstanceExtensionProperties" => enumerate_instance_extension_properties as unsafe extern "C" fn(*const c_char, *mut u32, *mut u8) -> VkResult,
        "vkCreateInstance" => create_instance as unsafe extern "C" fn(*const u8, *const c_void, *mut *mut InstanceObj) -> VkResult,
        "vkDestroyInstance" => destroy_instance as unsafe extern "C" fn(*mut InstanceObj, *const c_void),
        "vkEnumeratePhysicalDevices" => enumerate_physical_devices as unsafe extern "C" fn(*mut InstanceObj, *mut u32, *mut *mut PdevObj) -> VkResult,
        "vkGetPhysicalDeviceProperties" => get_properties as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceProperties2" => get_properties2 as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceProperties2KHR" => get_properties2 as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceFeatures" => get_features as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceFeatures2" => get_features2 as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceFeatures2KHR" => get_features2 as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceMemoryProperties" => get_memory_properties as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceMemoryProperties2" => get_memory_properties2 as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceMemoryProperties2KHR" => get_memory_properties2 as unsafe extern "C" fn(*mut c_void, *mut u8),
        "vkGetPhysicalDeviceFormatProperties" => get_format_properties as unsafe extern "C" fn(*mut c_void, u32, *mut u8),
        "vkGetPhysicalDeviceFormatProperties2" => get_format_properties2 as unsafe extern "C" fn(*mut c_void, u32, *mut u8),
        "vkGetPhysicalDeviceFormatProperties2KHR" => get_format_properties2 as unsafe extern "C" fn(*mut c_void, u32, *mut u8),
        "vkGetPhysicalDeviceQueueFamilyProperties" => get_queue_family_properties as unsafe extern "C" fn(*mut c_void, *mut u32, *mut u8),
        "vkGetPhysicalDeviceQueueFamilyProperties2" => get_queue_family_properties2 as unsafe extern "C" fn(*mut c_void, *mut u32, *mut u8),
        "vkGetPhysicalDeviceQueueFamilyProperties2KHR" => get_queue_family_properties2 as unsafe extern "C" fn(*mut c_void, *mut u32, *mut u8),
        "vkEnumerateDeviceExtensionProperties" => enumerate_device_extension_properties as unsafe extern "C" fn(*mut c_void, *const c_char, *mut u32, *mut u8) -> VkResult,
        "vkEnumerateDeviceLayerProperties" => enumerate_device_layer_properties as unsafe extern "C" fn(*mut c_void, *mut u32, *mut c_void) -> VkResult,
        "vkGetPhysicalDeviceExternalBufferProperties" => external_properties_none as unsafe extern "C" fn(*mut c_void, *const u8, *mut u8),
        "vkGetPhysicalDeviceExternalBufferPropertiesKHR" => external_properties_none as unsafe extern "C" fn(*mut c_void, *const u8, *mut u8),
        "vkGetPhysicalDeviceExternalSemaphoreProperties" => external_properties_none as unsafe extern "C" fn(*mut c_void, *const u8, *mut u8),
        "vkGetPhysicalDeviceExternalSemaphorePropertiesKHR" => external_properties_none as unsafe extern "C" fn(*mut c_void, *const u8, *mut u8),
        "vkGetPhysicalDeviceExternalFenceProperties" => external_properties_none as unsafe extern "C" fn(*mut c_void, *const u8, *mut u8),
        "vkGetPhysicalDeviceExternalFencePropertiesKHR" => external_properties_none as unsafe extern "C" fn(*mut c_void, *const u8, *mut u8),
        "vkGetPhysicalDeviceImageFormatProperties" => get_image_format_properties as unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, *mut u8) -> VkResult,
        "vkGetPhysicalDeviceSparseImageFormatProperties" => get_sparse_image_format_properties as unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, *mut u32, *mut u8),
        "vkGetDeviceProcAddr" => get_device_proc_addr as unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_void,
        "vkGetPhysicalDeviceSurfaceSupportKHR" => surface_support as unsafe extern "C" fn(*mut c_void, u32, u64, *mut u32) -> VkResult,
        "vkGetPhysicalDeviceSurfaceCapabilitiesKHR" => surface_capabilities as unsafe extern "C" fn(*mut c_void, u64, *mut u8) -> VkResult,
        "vkGetPhysicalDeviceSurfaceFormatsKHR" => surface_formats as unsafe extern "C" fn(*mut c_void, u64, *mut u32, *mut u8) -> VkResult,
        "vkGetPhysicalDeviceSurfacePresentModesKHR" => surface_present_modes as unsafe extern "C" fn(*mut c_void, u64, *mut u32, *mut u32) -> VkResult,
        "vkCreateWaylandSurfaceKHR" => create_wayland_surface as unsafe extern "C" fn(u64, *const u8, *const c_void, *mut u64) -> VkResult,
        "vkCreateXcbSurfaceKHR" => create_xcb_surface as unsafe extern "C" fn(u64, *const u8, *const c_void, *mut u64) -> VkResult,
        "vkCreateXlibSurfaceKHR" => create_xlib_surface as unsafe extern "C" fn(u64, *const u8, *const c_void, *mut u64) -> VkResult,
        "vkCreateHeadlessSurfaceEXT" => create_headless_surface as unsafe extern "C" fn(u64, *const u8, *const c_void, *mut u64) -> VkResult,
        "vkDestroySurfaceKHR" => destroy_surface as unsafe extern "C" fn(u64, u64, *const c_void),
        "vkGetPhysicalDeviceWaylandPresentationSupportKHR" => wayland_presentation_support as unsafe extern "C" fn(*mut c_void, u32, u64) -> u32,
        "vkGetPhysicalDeviceXcbPresentationSupportKHR" => xcb_presentation_support as unsafe extern "C" fn(*mut c_void, u32, u64, u32) -> u32,
        "vkGetPhysicalDeviceXlibPresentationSupportKHR" => xlib_presentation_support as unsafe extern "C" fn(*mut c_void, u32, u64, u64) -> u32,
        "vkCreateSwapchainKHR" => swapchain::create_swapchain as unsafe extern "C" fn(u64, *const u8, *const c_void, *mut u64) -> VkResult,
        "vkDestroySwapchainKHR" => swapchain::destroy_swapchain as unsafe extern "C" fn(u64, u64, *const c_void),
        "vkGetSwapchainImagesKHR" => swapchain::get_swapchain_images as unsafe extern "C" fn(u64, u64, *mut u32, *mut u64) -> VkResult,
        "vkAcquireNextImageKHR" => swapchain::acquire_next_image as unsafe extern "C" fn(u64, u64, u64, u64, u64, *mut u32) -> VkResult,
        "vkAcquireNextImage2KHR" => swapchain::acquire_next_image2 as unsafe extern "C" fn(u64, *const u8, *mut u32) -> VkResult,
        "vkQueuePresentKHR" => swapchain::queue_present as unsafe extern "C" fn(u64, *const u8) -> VkResult,
        "vkMapMemory" => fwd::map_memory as unsafe extern "C" fn(u64, u64, u64, u64, u32, *mut *mut u8) -> VkResult,
        "vkUnmapMemory" => fwd::unmap_memory as unsafe extern "C" fn(u64, u64),
        "vkFlushMappedMemoryRanges" => fwd::flush_ranges as unsafe extern "C" fn(u64, u32, *const u8) -> VkResult,
        "vkInvalidateMappedMemoryRanges" => fwd::invalidate_ranges as unsafe extern "C" fn(u64, u32, *const u8) -> VkResult,
    )
}

#[no_mangle]
pub unsafe extern "C" fn vk_icdGetInstanceProcAddr(_instance: *mut c_void, name: *const c_char) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    match CStr::from_ptr(name).to_str() {
        Ok(n) => {
            let p = proc_addr_by_name(n);
            if std::env::var("GPU_ICD_DEBUG_PROC").is_ok() {
                eprintln!("[gpu-icd] proc {n}: {}", if p.is_null() { "NULL" } else { "ok" });
            }
            p
        }
        Err(_) => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn vk_icdGetPhysicalDeviceProcAddr(_instance: *mut c_void, name: *const c_char) -> *const c_void {
    vk_icdGetInstanceProcAddr(std::ptr::null_mut(), name)
}

#[no_mangle]
pub unsafe extern "C" fn vk_icdNegotiateLoaderICDInterfaceVersion(version: *mut u32) -> VkResult {
    match version.as_mut() {
        Some(v) => {
            *v = (*v).min(ICD_INTERFACE_VERSION);
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_functions_resolve_and_unknown_do_not() {
        for n in ["vkCreateInstance", "vkEnumeratePhysicalDevices", "vkGetPhysicalDeviceProperties2", "vkEnumerateInstanceVersion", "vkGetPhysicalDeviceQueueFamilyProperties"] {
            assert!(!proc_addr_by_name(n).is_null(), "{n}");
        }
        // Polecenia przekazywane generycznie do serwera.
        for n in ["vkCmdDraw", "vkQueueSubmit", "vkCreateDevice", "vkCreateBuffer", "vkWaitForFences"] {
            assert!(!proc_addr_by_name(n).is_null(), "{n}");
        }
        for n in ["vkGetPhysicalDeviceSurfaceSupportKHR", "vkGetPhysicalDeviceSurfaceCapabilitiesKHR", "vkGetPhysicalDeviceSurfaceFormatsKHR", "vkGetPhysicalDeviceSurfacePresentModesKHR"] {
            assert!(!proc_addr_by_name(n).is_null(), "{n}");
        }
        for n in ["vkCreateSwapchainKHR", "vkDestroySwapchainKHR", "vkGetSwapchainImagesKHR", "vkAcquireNextImageKHR", "vkQueuePresentKHR"] {
            assert!(!proc_addr_by_name(n).is_null(), "{n}");
        }
        assert_ne!(proc_addr_by_name("vkCmdDraw"), proc_addr_by_name("vkCmdDrawIndexed"), "każde polecenie ma własny punkt wejścia");
        for n in ["", "vkCreateInstanceX", "vkNieMaTakiego", "vkCreateDisplayPlaneSurfaceKHR", "vkGetPhysicalDeviceSurfaceCapabilities2KHR", "vkGetDeviceGroupPresentCapabilitiesKHR"] {
            assert!(proc_addr_by_name(n).is_null(), "{n}");
        }
    }

    #[test]
    fn negotiated_version_is_capped_at_5_and_null_is_rejected() {
        let mut v = 7u32;
        assert_eq!(unsafe { vk_icdNegotiateLoaderICDInterfaceVersion(&mut v) }, VK_SUCCESS);
        assert_eq!(v, 5);
        let mut v = 3u32;
        unsafe { vk_icdNegotiateLoaderICDInterfaceVersion(&mut v) };
        assert_eq!(v, 3);
        assert_ne!(unsafe { vk_icdNegotiateLoaderICDInterfaceVersion(std::ptr::null_mut()) }, VK_SUCCESS);
    }

    #[test]
    fn api_version_is_clamped_to_1_3() {
        let v14 = (1 << 22) | (4 << 12) | 312;
        assert_eq!(clamp_api_version(v14), MAX_API_VERSION);
        let v12 = (1 << 22) | (2 << 12) | 100;
        assert_eq!(clamp_api_version(v12), v12, "starsze wersje bez zmian");
    }

    #[test]
    fn instance_version_is_1_3() {
        let mut v = 0u32;
        assert_eq!(unsafe { enumerate_instance_version(&mut v) }, VK_SUCCESS);
        assert_eq!((v >> 22, (v >> 12) & 0x3ff), (1, 3));
    }

    #[test]
    fn walk_chain_follows_pnext_and_stops_on_cycles() {
        // Trzy struktury po 16 B: sType u32 @0, pNext @8.
        let mut a = [0u64; 2];
        let mut b = [0u64; 2];
        let mut c = [0u64; 2];
        a[0] = 11;
        b[0] = 22;
        c[0] = 33;
        a[1] = b.as_mut_ptr() as u64;
        b[1] = c.as_mut_ptr() as u64;
        let chain = unsafe { walk_chain(a.as_mut_ptr() as *mut u8) };
        assert_eq!(chain.iter().map(|x| x.1).collect::<Vec<_>>(), vec![22, 33]);
        c[1] = b.as_mut_ptr() as u64; // cykl b -> c -> b
        std::hint::black_box(&c);
        let chain = unsafe { walk_chain(a.as_mut_ptr() as *mut u8) };
        assert_eq!(chain.len(), MAX_CHAIN_WALK, "cykl nie zawiesza ICD");
    }

    #[test]
    fn create_instance_without_configuration_fails_cleanly() {
        std::env::remove_var("GPU_ICD_ADDR");
        let info = [0u64; 8];
        let mut out: *mut InstanceObj = std::ptr::null_mut();
        assert_eq!(unsafe { create_instance(info.as_ptr() as *const u8, std::ptr::null(), &mut out) }, VK_ERROR_INITIALIZATION_FAILED);
        assert!(out.is_null());
        let mut with_ext = [0u64; 8];
        with_ext[6] = 1; // enabledExtensionCount @48
        assert_eq!(unsafe { create_instance(with_ext.as_ptr() as *const u8, std::ptr::null(), &mut out) }, VK_ERROR_EXTENSION_NOT_PRESENT);
    }
    #[test]
    fn external_handle_queries_report_no_support_and_keep_the_header() {
        // sType, pNext i pole `externalMemoryFeatures`/typy: bajty od 16 wyzerowane, nagłówek nietknięty.
        let mut out = [0xAAu8; 40];
        unsafe { external_properties_none(std::ptr::null_mut(), std::ptr::null(), out.as_mut_ptr()) };
        assert!(out[..16].iter().all(|b| *b == 0xAA));
        assert!(out[16..28].iter().all(|b| *b == 0));
        assert!(out[28..].iter().all(|b| *b == 0xAA));
        unsafe { external_properties_none(std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut()) };
        for name in ["vkGetPhysicalDeviceExternalBufferProperties", "vkGetPhysicalDeviceExternalSemaphorePropertiesKHR", "vkGetPhysicalDeviceExternalFenceProperties"] {
            assert!(!local_proc_addr(name).is_null(), "{name}");
        }
    }

    #[test]
    fn instance_extensions_include_the_external_capability_queries_dxvk_requires() {
        let names: Vec<&str> = wsi::advertised_instance_extensions(&[]).iter().map(|(n, _)| *n).collect();
        for n in ["VK_KHR_external_memory_capabilities", "VK_KHR_external_semaphore_capabilities", "VK_KHR_external_fence_capabilities"] {
            assert!(names.contains(&n), "{n}");
        }
    }

}
