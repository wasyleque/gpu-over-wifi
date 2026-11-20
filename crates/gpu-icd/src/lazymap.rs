//! Odczyt leniwy odwzorowanej pamięci (M3). Po zakończeniu pracy GPU strony, które GPU mogło zapisać, nie są pobierane z serwera:
//! tylko oznaczamy je jako nieaktualne i zrzucamy z widoku aplikacji (`madvise(MADV_DONTNEED)` na podwójnym odwzorowaniu `memfd`,
//! patrz `shadow`). Kiedy aplikacja dotknie takiej strony, jądro zgłasza błąd strony typu MINOR przez `userfaultfd`, wątek obsługi
//! pobiera okno stron z serwera, wpisuje je przez widok własny i kończy błąd `UFFDIO_CONTINUE`. Aplikacja widzi dokładnie te same
//! dane, które dałaby synchroniczna kopia po oczekiwaniu, ale płaci tylko za strony, które naprawdę czyta.
//!
//! Zakleszczenia: wątek obsługi pobiera dane przez `fetch` (blokada instancji + obieg po sieci). Jeśli wątek aplikacji, który dotknął
//! strony, sam trzyma blokadę instancji (np. przekazał do naszego kodu wskaźnik do odwzorowanej pamięci), `fetch` czeka w
//! nieskończoność; dlatego `fetch` używa `try_lock` z limitem czasu i przy porażce oddaje stronę z dotychczasową zawartością
//! (licznik `FETCH_TIMEOUTS`), zamiast blokować grę na zawsze.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::lazy::LazyPages;

pub const PAGE: usize = 4096;
/// Okno pobierania (strony, potęga dwójki): zaczyna od 64 KiB, przy odczycie sekwencyjnym rośnie do 16 MiB.
pub const FETCH_WINDOW_MIN: usize = 16;
pub const FETCH_WINDOW_MAX: usize = 4096;

/// Najmniejsze okno pobrania w stronach (`GPU_ICD_LAZY_WINDOW_MIN`, potęga dwójki 16..4096): większe okno = mniej obiegów przy odczytach rozproszonych.
fn window_min() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("GPU_ICD_LAZY_WINDOW_MIN").ok().and_then(|v| v.parse::<usize>().ok()).filter(|v| v.is_power_of_two() && (FETCH_WINDOW_MIN..=FETCH_WINDOW_MAX).contains(v)).unwrap_or(FETCH_WINDOW_MIN))
}

const SYS_USERFAULTFD: i64 = 323;
const O_CLOEXEC: i64 = 0o2000000;
const O_NONBLOCK: i64 = 0o4000;
const UFFD_USER_MODE_ONLY: i64 = 1;
const UFFD_API: u64 = 0xAA;
const UFFD_FEATURE_MINOR_SHMEM: u64 = 1 << 10;
const UFFD_FEATURE_WP_HUGETLBFS_SHMEM: u64 = 1 << 12;
const UFFD_FEATURE_WP_UNPOPULATED: u64 = 1 << 13;
const UFFD_FEATURE_WP_ASYNC: u64 = 1 << 15;
const UFFDIO_API: u64 = 0xc018_aa3f;
const UFFDIO_REGISTER: u64 = 0xc020_aa00;
const UFFDIO_WRITEPROTECT: u64 = 0xc018_aa06;
const UFFDIO_CONTINUE: u64 = 0xc020_aa07;
const UFFDIO_WAKE: u64 = 0x8010_aa02;
const UFFDIO_REGISTER_MODE_WP: u64 = 2;
const UFFDIO_REGISTER_MODE_MINOR: u64 = 4;
const UFFDIO_WRITEPROTECT_MODE_WP: u64 = 1;
const UFFD_EVENT_PAGEFAULT: u8 = 0x12;
const POLLIN: i16 = 1;

extern "C" {
    fn syscall(num: i64, ...) -> i64;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn poll(fds: *mut PollFd, n: u64, timeout: i32) -> i32;
    #[link_name = "read"]
    fn sys_read(fd: i32, buf: *mut std::ffi::c_void, n: usize) -> isize;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[repr(C)]
struct UffdioApi {
    api: u64,
    features: u64,
    ioctls: u64,
}

#[repr(C)]
struct UffdioRegister {
    start: u64,
    len: u64,
    mode: u64,
    ioctls: u64,
}

#[repr(C)]
struct UffdioWriteProtect {
    start: u64,
    len: u64,
    mode: u64,
}

#[repr(C)]
struct UffdioContinue {
    start: u64,
    len: u64,
    mode: u64,
    mapped: i64,
}

/// Jak często `fetch` nie dostał blokady instancji w limicie czasu (strona oddana bez pobrania).
pub static FETCH_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
/// Ile błędów stron obsłużono i ile z nich wymagało obiegu po sieci.
/// Nieudane `UFFDIO_CONTINUE` (wątek aplikacji zostałby uśpiony w jądrze na zawsze): liczba i ostatni kod errno.
pub static CONTINUE_FAILS: AtomicU64 = AtomicU64::new(0);
pub static FAULTS: AtomicU64 = AtomicU64::new(0);
pub static FAULTS_FETCHED: AtomicU64 = AtomicU64::new(0);
pub static FETCHED_BYTES: AtomicU64 = AtomicU64::new(0);
/// Czas obsługi pobrań z serwera przy błędach stron (ns): skan zapisów aplikacji + obieg po sieci; cały czas stoi wątek aplikacji.
pub static FETCH_NS: AtomicU64 = AtomicU64::new(0);
pub static FETCH_FLUSH_NS: AtomicU64 = AtomicU64::new(0);
pub static FETCH_CALL_NS: AtomicU64 = AtomicU64::new(0);
pub static FETCH_LOCK_NS: AtomicU64 = AtomicU64::new(0);
pub static FETCH_WORK_NS: AtomicU64 = AtomicU64::new(0);

/// Stan jednej leniwej pamięci.
struct State {
    stale: LazyPages,
    /// Strony aktualnie odwzorowane w widoku aplikacji (bity): tylko je trzeba zrzucać przy kolejnym unieważnieniu.
    present: Vec<u64>,
    /// Zmienia się przy każdym unieważnieniu; wątek obsługi powtarza pobranie, jeśli zmieniło się w trakcie.
    epoch: u64,
    /// Rozmiar następnego okna pobierania (strony, potęga dwójki) i koniec poprzedniego: odczyt sekwencyjny podwaja okno, skok je zeruje.
    window: usize,
    last_end: usize,
    /// Strony zrzucone z widoku aplikacji przez `invalidate`: stan "zapisane" (flaga jądra) znika razem z wpisem tablicy stron, więc zapis aplikacji
    /// do takiej strony tuż przed zrzutem nie byłby widoczny w skanie i nigdy nie dotarłby na serwer (GPU czytałoby stare argumenty rysowania pośredniego
    /// i wieszało się). Te strony skan musi więc zawsze zweryfikować porównaniem skrótów (patrz `take_recheck`).
    recheck: Vec<(usize, usize)>,
    /// Okna (początek bajtowy → długość w stronach) pobierane przy prawdziwych błędach stron, z licznikiem: kandydaci do pobrania z wyprzedzeniem.
    hot: std::collections::HashMap<(usize, u64), (usize, u32)>,
}

pub struct LazyRegion {
    prefetch_queued: std::sync::atomic::AtomicBool,
    /// Identyfikator pamięci w protokole (dla `fetch`).
    pub memory: u64,
    /// Instancja sterownika (`InstanceObj`), do której należy pamięć: pobranie idzie jej połączeniem.
    pub owner: usize,
    app: usize,
    own: usize,
    len: usize,
    state: Mutex<State>,
}

/// Pobranie zakresu bajtów `[offset, offset + len)` z serwera i wpisanie go przez `LazyRegion::fill`. `false` = nie udało się.
pub type Fetch = fn(&LazyRegion, usize, usize) -> bool;

struct Hub {
    uffd: OwnedFd,
    regions: RwLock<BTreeMap<usize, Arc<LazyRegion>>>,
    fetch: OnceLock<Fetch>,
}

static HUB: OnceLock<Option<Arc<Hub>>> = OnceLock::new();

fn hub() -> Option<Arc<Hub>> {
    HUB.get_or_init(|| {
        // SAFETY: wywołania systemowe na poprawnych strukturach; deskryptor przejmuje `OwnedFd`.
        let uffd = unsafe {
            let fd = syscall(SYS_USERFAULTFD, O_CLOEXEC | O_NONBLOCK | UFFD_USER_MODE_ONLY);
            if fd < 0 {
                return None;
            }
            OwnedFd::from_raw_fd(fd as i32)
        };
        let mut api = UffdioApi {
            api: UFFD_API,
            features: UFFD_FEATURE_MINOR_SHMEM | UFFD_FEATURE_WP_ASYNC | UFFD_FEATURE_WP_UNPOPULATED | UFFD_FEATURE_WP_HUGETLBFS_SHMEM,
            ioctls: 0,
        };
        // SAFETY: `api` żyje przez czas wywołania.
        if unsafe { ioctl(uffd.as_raw_fd(), UFFDIO_API, &mut api as *mut UffdioApi) } < 0 {
            return None;
        }
        let hub = Arc::new(Hub { uffd, regions: RwLock::new(BTreeMap::new()), fetch: OnceLock::new() });
        let h = hub.clone();
        std::thread::Builder::new().name("gpu-icd-lazy".into()).spawn(move || handler(h)).ok()?;
        Some(hub)
    })
    .clone()
}

/// Czy w tym procesie działa odczyt leniwy (jądro obsługuje MINOR dla shmem).
pub fn available() -> bool {
    hub().is_some()
}

/// Ustawia funkcję pobierającą dane z serwera (raz).
pub fn set_fetch(f: Fetch) {
    if let Some(h) = hub() {
        let _ = h.fetch.set(f);
    }
}

impl LazyRegion {
    /// Rejestruje podwójne odwzorowanie `app`/`own` (`len` wielokrotność strony, strony `own` muszą być już w pamięci podręcznej stron,
    /// czyli dotknięte). Zwraca `None`, gdy jądro odmawia (wtedy wołający zostaje przy zwykłej pamięci).
    pub fn register(memory: u64, owner: usize, app: *mut u8, own: *mut u8, len: usize) -> Option<Arc<LazyRegion>> {
        let h = hub()?;
        let (a, o) = (app as usize, own as usize);
        if !a.is_multiple_of(PAGE) || !len.is_multiple_of(PAGE) || len == 0 {
            return None;
        }
        let mut reg = UffdioRegister { start: a as u64, len: len as u64, mode: UFFDIO_REGISTER_MODE_MINOR | UFFDIO_REGISTER_MODE_WP, ioctls: 0 };
        // SAFETY: zakres to żywe odwzorowanie `MAP_SHARED` o rozmiarze `len`.
        if unsafe { ioctl(h.uffd.as_raw_fd(), UFFDIO_REGISTER, &mut reg as *mut UffdioRegister) } < 0 {
            return None;
        }
        let mut wp = UffdioWriteProtect { start: a as u64, len: len as u64, mode: UFFDIO_WRITEPROTECT_MODE_WP };
        // SAFETY: jak wyżej.
        if unsafe { ioctl(h.uffd.as_raw_fd(), UFFDIO_WRITEPROTECT, &mut wp as *mut UffdioWriteProtect) } < 0 {
            return None;
        }
        let pages = len / PAGE;
        let region = Arc::new(LazyRegion { prefetch_queued: std::sync::atomic::AtomicBool::new(false), memory, owner, app: a, own: o, len, state: Mutex::new(State { stale: LazyPages::new(len), present: vec![0; pages.div_ceil(64)], epoch: 0, window: window_min(), last_end: usize::MAX, recheck: Vec::new(), hot: Default::default() }) });
        h.regions.write().unwrap_or_else(|e| e.into_inner()).insert(a, region.clone());
        Some(region)
    }

    /// Wyrejestrowuje region (przed zwolnieniem odwzorowań).
    pub fn unregister(&self) {
        if let Some(h) = hub() {
            h.regions.write().unwrap_or_else(|e| e.into_inner()).remove(&self.app);
        }
    }

    pub fn app_base(&self) -> usize {
        self.app
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Wpisuje dane przez widok własny (nie wywołuje błędów stron).
    pub fn fill(&self, offset: usize, data: &[u8]) {
        let end = offset.saturating_add(data.len()).min(self.len);
        if offset >= end {
            return;
        }
        // SAFETY: zakres leży w odwzorowaniu `own` o rozmiarze `len`.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), (self.own + offset) as *mut u8, end - offset) };
    }

    /// GPU mogło zapisać `ranges` (offset, długość): oznacz jako nieaktualne i zrzuć z widoku aplikacji strony, które w nim są.
    pub fn invalidate(&self, ranges: &[(u64, u64)]) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.epoch += 1;
        st.stale.invalidate(ranges);
        let pages = self.len / PAGE;
        for &(off, len) in ranges {
            if len == 0 || off as usize >= self.len {
                continue;
            }
            let first = off as usize / PAGE;
            let last = ((off.saturating_add(len)).min(self.len as u64) as usize - 1) / PAGE;
            let mut p = first;
            while p <= last.min(pages - 1) {
                if st.present[p / 64] & (1 << (p % 64)) == 0 {
                    p += 1;
                    continue;
                }
                // Scalony bieg obecnych stron: jedno wywołanie `madvise`.
                let start = p;
                while p <= last.min(pages - 1) && st.present[p / 64] & (1 << (p % 64)) != 0 {
                    st.present[p / 64] &= !(1 << (p % 64));
                    p += 1;
                }
                // SAFETY: zakres leży w widoku aplikacji; `MADV_DONTNEED` na `MAP_SHARED` zostawia dane w pliku `memfd`.
                unsafe { madvise((self.app + start * PAGE) as *mut u8, (p - start) * PAGE, 4) };
                st.recheck.push((start * PAGE, p * PAGE));
            }
        }
    }

    /// Zakresy (bajty względem początku regionu) zrzucone od ostatniego wywołania: skan zapisów aplikacji musi je zweryfikować skrótami, bo flaga
    /// "zapisane" po zrzucie jest utracona. Opróżnia listę.
    pub fn take_recheck(&self) -> Vec<(usize, usize)> {
        std::mem::take(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()).recheck)
    }

    /// Dane `[from, to)` są aktualne (np. po jawnym `vkInvalidateMappedMemoryRanges`).
    pub fn mark_fresh(&self, from: usize, to: usize) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).stale.mark_fresh(from, to);
    }

    pub fn stale_pages(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).stale.stale_count()
    }
}

fn handler(hub: Arc<Hub>) {
    let fd = hub.uffd.as_raw_fd();
    let mut msg = [0u8; 32];
    loop {
        let mut p = PollFd { fd, events: POLLIN, revents: 0 };
        // SAFETY: `p` żyje przez czas wywołania.
        let n = unsafe { poll(&mut p as *mut PollFd, 1, 1000) };
        if n <= 0 {
            continue;
        }
        // SAFETY: bufor ma 32 B, czyli rozmiar `struct uffd_msg`.
        let got = unsafe { sys_read(fd, msg.as_mut_ptr() as *mut std::ffi::c_void, msg.len()) };
        if got != 32 || msg[0] != UFFD_EVENT_PAGEFAULT {
            continue;
        }
        let addr = u64::from_le_bytes(msg[16..24].try_into().unwrap_or([0; 8])) as usize;
        serve_fault(&hub, addr);
        // Diagnostyka: czy w czasie obsługi czekały już kolejne błędy stron (obsługa jest szeregowa: jeden wątek, jedno pobranie naraz).
        let mut p2 = PollFd { fd, events: POLLIN, revents: 0 };
        // SAFETY: `p2` żyje przez czas wywołania.
        if unsafe { poll(&mut p2 as *mut PollFd, 1, 0) } > 0 {
            FAULT_BACKLOG.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Ile razy po obsłużeniu błędu strony czekał już następny.
/// Klucz ostatnio rozpoczętego oczekiwania (uchwyt pierwszego semafora/ogrodzenia): do kojarzenia okien odczytu z oczekiwaniem, po którym są czytane.
pub static LAST_WAIT_KEY: AtomicU64 = AtomicU64::new(0);
pub static FAULT_BACKLOG: AtomicU64 = AtomicU64::new(0);

/// `UFFDIO_CONTINUE` z obsługą błędu. Jego porażka (np. brak strony w pamięci podręcznej stron widoku własnego, gdy pobranie się nie udało) zostawiała
/// wątek aplikacji uśpiony w `handle_userfault` bez końca (zawieszenie Cyberpunka 2026-11-19, stos: redDispatcher8 w jądrze, gpu-icd-lazy w poll).
/// Po porażce: dotknięcie strony widoku własnego (odczyt+zapis tej samej wartości wprowadza ją do pamięci podręcznej), ponowienie i na końcu
/// `UFFDIO_WAKE`, żeby wątek chociaż spróbował ponownie. Porażki idą do logu (pierwsze 8).
fn cont(hub: &Hub, start: usize, len: usize, own: Option<usize>) {
    let fd = hub.uffd.as_raw_fd();
    let mut c = UffdioContinue { start: start as u64, len: len as u64, mode: 0, mapped: 0 };
    // SAFETY: `c` żyje przez czas wywołania; zakres leży w zarejestrowanym odwzorowaniu.
    let r = unsafe { ioctl(fd, UFFDIO_CONTINUE, &mut c as *mut UffdioContinue) };
    if r >= 0 && c.mapped as u64 >= len as u64 {
        return;
    }
    if r >= 0 || std::io::Error::last_os_error().raw_os_error() == Some(17) {
        // Częściowy sukces albo EEXIST: jądro budzi wątki tylko dla stron faktycznie odwzorowanych przez to wywołanie, a zakres staje na pierwszej
        // stronie już odwzorowanej, więc strona błędu leżąca za nią zostawała bez wybudzenia (zawieszenie Cyberpunka 2026-11-19, gdb po dołączeniu
        // ruszał wątek). Odwzorowujemy resztę strona po stronie (EEXIST pomijamy) i budzimy cały zakres.
        let mut p = start;
        while p < start + len {
            let mut one = UffdioContinue { start: p as u64, len: PAGE as u64, mode: 0, mapped: 0 };
            // SAFETY: jak wyżej.
            unsafe { ioctl(fd, UFFDIO_CONTINUE, &mut one as *mut UffdioContinue) };
            p += PAGE;
        }
        let mut range = [start as u64, len as u64];
        // SAFETY: `range` to `struct uffdio_range`.
        unsafe { ioctl(fd, UFFDIO_WAKE, range.as_mut_ptr()) };
        return;
    }
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let n = CONTINUE_FAILS.fetch_add(1, Ordering::Relaxed);
    if n < 8 {
        eprintln!("[gpu-icd] UFFDIO_CONTINUE nie powiodło się: adres {start:#x}, długość {len:#x}, errno {err}, odwzorowano {}", c.mapped);
    }
    if let Some(o) = own {
        let mut p = o;
        while p < o + len {
            // SAFETY: adres leży w widoku własnym regionu (ten sam offset co w widoku aplikacji); wpisujemy tę samą wartość.
            unsafe {
                let b = p as *mut u8;
                std::ptr::write_volatile(b, std::ptr::read_volatile(b));
            }
            p += PAGE;
        }
        let mut c2 = UffdioContinue { start: start as u64, len: len as u64, mode: 0, mapped: 0 };
        // SAFETY: jak wyżej.
        if unsafe { ioctl(fd, UFFDIO_CONTINUE, &mut c2 as *mut UffdioContinue) } >= 0 {
            return;
        }
    }
    let mut range = [start as u64, len as u64];
    // SAFETY: `range` to `struct uffdio_range`.
    unsafe { ioctl(fd, UFFDIO_WAKE, range.as_mut_ptr()) };
}

/// Diagnostyka (`GPU_ICD_FETCHLOG=ścieżka`): dopisuje „czas_µs pamięć offset długość” przy każdym pobraniu; do analizy powtarzalności odczytów.
fn fetch_log(memory: u64, off: usize, len: usize, data: Option<usize>) {
    use std::io::Write;
    static F: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    let f = F.get_or_init(|| std::env::var("GPU_ICD_FETCHLOG").ok().and_then(|p| std::fs::File::create(p).ok()).map(Mutex::new));
    if let Some(f) = f {
        let t = T0.get_or_init(std::time::Instant::now).elapsed().as_micros();
        let mut w = f.lock().unwrap_or_else(|e| e.into_inner());
        // Skrót zawartości pobranego okna: czy dane różnią się od poprzedniego pobrania tego samego okna (diagnostyka nadmiarowych unieważnień).
        let h = data.map(|p| {
            // SAFETY: okno `[p, p+len)` właśnie wpisano w widok własny regionu (żywe odwzorowanie).
            let sl = unsafe { std::slice::from_raw_parts(p as *const u64, len / 8) };
            sl.iter().fold(0xcbf29ce484222325u64, |h, &w| (h ^ w).wrapping_mul(0x100000001b3).rotate_left(5))
        });
        let _ = writeln!(w, "{t} {memory:x} {off} {len} {:x} {:x}", h.unwrap_or(0), LAST_WAIT_KEY.load(Ordering::Relaxed));
    }
}


/// Pobiera okno wokół `off` (wspólne dla błędu strony i pobrania z wyprzedzeniem). `pre` = Some(strony): okno narzucone, bez zmiany stanu okna
/// adaptacyjnego i bez zliczania gorących okien. Zwraca, czy strona `off` nadal wymaga odwzorowania przez wołającego.
fn fetch_for(hub: &Hub, r: &Arc<LazyRegion>, off: usize, pre: Option<usize>) -> bool {
    let mut continue_fault_page = true;
    // Do 3 prób: unieważnienie w trakcie pobierania unieważnia jego wynik (dane mogły się zdezaktualizować).
    for _ in 0..3 {
        let (plan, epoch) = {
            let mut st = r.state.lock().unwrap_or_else(|e| e.into_inner());
            let page = off / PAGE;
            if !st.stale.is_stale(page) {
                break; // strona świeża (zrzucona z widoku): wystarczy ją odwzorować, okno zostaje bez zmian
            }
            let window = match pre {
                Some(w) => w,
                None => {
                    // Błąd tuż za poprzednim oknem = odczyt sekwencyjny: okno rośnie; w przeciwnym razie wraca do minimum.
                    st.window = if page == st.last_end { (st.window * 2).min(FETCH_WINDOW_MAX) } else { window_min() };
                    st.window
                }
            };
            let plan = st.stale.plan_fetch(off, window);
            if pre.is_none() {
                if let Some(&(_, b)) = plan.last() {
                    st.last_end = b / PAGE;
                }
                if let Some(&(a, b)) = plan.first() {
                    let e = st.hot.entry((a, LAST_WAIT_KEY.load(Ordering::Relaxed))).or_insert((0, 0));
                    *e = ((b - a) / PAGE, e.1 + 1);
                    if st.hot.len() > 256 {
                        st.hot.clear();
                    }
                }
            }
            (plan, st.epoch)
        };
        if plan.is_empty() {
            break;
        }
        let Some(fetch) = hub.fetch.get() else { break };
        let mut ok = true;
        let t_fetch = std::time::Instant::now();
        for &(a, b) in &plan {
            ok &= fetch(r, a, b - a);
            fetch_log(r.memory, a, b - a, if ok { Some(r.own + a) } else { None });
            FETCHED_BYTES.fetch_add((b - a) as u64, Ordering::Relaxed);
        }
        if pre.is_some() {
            PREFETCH_NS.fetch_add(t_fetch.elapsed().as_nanos() as u64, Ordering::Relaxed);
            PREFETCHED.fetch_add(1, Ordering::Relaxed);
        } else {
            FETCH_NS.fetch_add(t_fetch.elapsed().as_nanos() as u64, Ordering::Relaxed);
            FAULTS_FETCHED.fetch_add(1, Ordering::Relaxed);
        }
        if !ok {
            break;
        }
        // Oznaczenie świeżości, bity `present` i odwzorowanie stron dzieją się pod blokadą stanu, więc `invalidate` (też pod nią) nie
        // wcisnie się między nie: inaczej odwzorowanie stałoby się po zrzucie i aplikacja widziałaby nieaktualne dane bez błędu strony.
        let mut st = r.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.epoch == epoch {
            for &(a, b) in &plan {
                st.stale.mark_fresh(a, b);
                for p in a / PAGE..b.div_ceil(PAGE) {
                    st.present[p / 64] |= 1 << (p % 64);
                }
                cont(hub, r.app + a, b - a, Some(r.own + a));
                if (a..b).contains(&off) {
                    continue_fault_page = false;
                }
            }
            break;
        }
    }
    continue_fault_page
}

pub static PREFETCHED: AtomicU64 = AtomicU64::new(0);
pub static PREFETCH_NS: AtomicU64 = AtomicU64::new(0);

/// Pobieranie z wyprzedzeniem (`GPU_ICD_PREFETCH=1`): po unieważnieniu pamięci osobny wątek pobiera okna, które aplikacja czytała przy poprzednich
/// błędach stron (≥ 3 razy), zanim aplikacja ich dotknie; błąd strony i obieg po sieci znikają z jej ścieżki krytycznej.
pub fn prefetch_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("GPU_ICD_PREFETCH").is_some())
}

struct Prefetcher {
    q: Mutex<Vec<Arc<LazyRegion>>>,
    cv: std::sync::Condvar,
}

fn prefetcher() -> &'static Prefetcher {
    static P: OnceLock<&'static Prefetcher> = OnceLock::new();
    P.get_or_init(|| {
        let p: &'static Prefetcher = Box::leak(Box::new(Prefetcher { q: Mutex::new(Vec::new()), cv: std::sync::Condvar::new() }));
        let _ = std::thread::Builder::new().name("gpu-icd-prefetch".into()).spawn(move || loop {
            let r = {
                let mut q = p.q.lock().unwrap_or_else(|e| e.into_inner());
                while q.is_empty() {
                    q = p.cv.wait(q).unwrap_or_else(|e| e.into_inner());
                }
                q.remove(0)
            };
            let Some(hub) = hub() else { continue };
            let mut hot: Vec<(usize, usize, u32)> = {
                let st = r.state.lock().unwrap_or_else(|e| e.into_inner());
                st.hot.iter().filter(|(_, v)| v.1 >= 3).map(|(&(a, _), v)| (a, v.0, v.1)).collect()
            };
            hot.sort_by(|a, b| b.2.cmp(&a.2));
            for (a, pages, _) in hot.into_iter().take(8) {
                fetch_for(&hub, &r, a, Some(pages));
            }
            r.prefetch_queued.store(false, Ordering::Release);
        });
        p
    })
}

impl LazyRegion {
    /// Gorące okna (początek bajtowy, długość w bajtach) czytane po oczekiwaniu o kluczu `key` przy ≥ `min` prawdziwych błędach stron (najwyżej 8, najczęstsze pierwsze).
    pub fn hot_windows(&self, min: u32, key: u64) -> Vec<(usize, usize)> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<(usize, usize, u32)> = st.hot.iter().filter(|(&(_, k), c)| k == key && c.1 >= min).map(|(&(a, _), c)| (a, c.0 * PAGE, c.1)).collect();
        v.sort_by(|a, b| b.2.cmp(&a.2));
        v.into_iter().take(8).map(|(a, l, _)| (a, l)).collect()
    }

    /// Dane `[off, off+len)` właśnie wpisano w widok własny (odpowiedź serwera dołączona do oczekiwania): oznacz je jako świeże i odwzoruj w widoku
    /// aplikacji, żeby pierwszy odczyt nie kończył się błędem strony. Wołać PO `invalidate` tego samego oczekiwania.
    pub fn install_fresh(&self, off: usize, len: usize) {
        let Some(h) = hub() else { return };
        let (a, b) = (off & !(PAGE - 1), (off + len).div_ceil(PAGE) * PAGE);
        if b > self.len || a >= b {
            return;
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.stale.mark_fresh(a, b);
        for p in a / PAGE..b / PAGE {
            st.present[p / 64] |= 1 << (p % 64);
        }
        cont(&h, self.app + a, b - a, Some(self.own + a));
    }

    /// Zleca pobranie gorących okien tej pamięci z wyprzedzeniem (po `invalidate`).
    pub fn prefetch_hot(self: &Arc<Self>) {
        if !prefetch_enabled() || self.prefetch_queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let p = prefetcher();
        p.q.lock().unwrap_or_else(|e| e.into_inner()).push(self.clone());
        p.cv.notify_one();
    }
}

fn serve_fault(hub: &Hub, addr: usize) {
    FAULTS.fetch_add(1, Ordering::Relaxed);
    let page_addr = addr & !(PAGE - 1);
    let region = {
        let map = hub.regions.read().unwrap_or_else(|e| e.into_inner());
        map.range(..=page_addr).next_back().filter(|(base, r)| page_addr < **base + r.len).map(|(_, r)| r.clone())
    };
    let mut continue_fault_page = true;
    if let Some(r) = region {
        let off = page_addr - r.app;
        continue_fault_page = fetch_for(hub, &r, off, None);
        if continue_fault_page {
            // Strona świeża albo pobranie się nie udało: odwzoruj samą stronę błędu (pod blokadą, z tego samego powodu co wyżej).
            let mut st = r.state.lock().unwrap_or_else(|e| e.into_inner());
            let p = off / PAGE;
            st.present[p / 64] |= 1 << (p % 64);
            cont(hub, page_addr, PAGE, Some(r.own + off));
        }
        return;
    }
    // Błąd w nieznanym regionie (już wyrejestrowany): zwolnij wątek, odwzorowując stronę.
    cont(hub, page_addr, PAGE, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::DoubleMap;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fake_fetch(r: &LazyRegion, off: usize, len: usize) -> bool {
        // "Serwer": strona n zawiera bajty (n + 1).
        let data: Vec<u8> = (0..len).map(|i| ((off + i) / PAGE + 1) as u8).collect();
        r.fill(off, &data);
        true
    }

    fn setup(pages: usize) -> Option<(DoubleMap, Arc<LazyRegion>)> {
        let dm = DoubleMap::new(pages * PAGE).ok()?;
        // Strony muszą być w pamięci podręcznej stron przed rejestracją (inaczej błąd "missing", nie "minor").
        for p in 0..pages {
            // SAFETY: zapis w granicach widoku własnego.
            unsafe { *dm.own_ptr().add(p * PAGE) = 0 };
        }
        if !available() {
            eprintln!("POMINIĘTO: brak userfaultfd MINOR dla shmem");
            return None;
        }
        set_fetch(fake_fetch);
        let r = LazyRegion::register(1, 0, dm.app_ptr(), dm.own_ptr(), dm.len())?;
        Some((dm, r))
    }

    #[test]
    fn reading_a_stale_page_fetches_a_window_and_returns_server_data() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dm, r)) = setup(256) else { return };
        r.invalidate(&[(0, (256 * PAGE) as u64)]);
        let before = FAULTS_FETCHED.load(Ordering::Relaxed);
        // SAFETY: odczyt w granicach widoku aplikacji.
        let v = unsafe { *dm.app_ptr().add(5 * PAGE + 7) };
        assert_eq!(v, 6, "strona 5 przychodzi z serwera");
        // Strona w tym samym oknie 16 stron: bez kolejnego pobrania.
        let w = unsafe { *dm.app_ptr().add(9 * PAGE) };
        assert_eq!(w, 10);
        assert_eq!(FAULTS_FETCHED.load(Ordering::Relaxed) - before, 1, "jedno pobranie na okno");
        // Strona z innego okna: drugie pobranie.
        let x = unsafe { *dm.app_ptr().add(200 * PAGE) };
        assert_eq!(x, 201);
        assert_eq!(FAULTS_FETCHED.load(Ordering::Relaxed) - before, 2);
        assert!(r.stale_pages() < 256);
        r.unregister();
    }

    #[test]
    fn a_second_invalidation_makes_the_next_read_fetch_again() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dm, r)) = setup(64) else { return };
        r.invalidate(&[(0, (64 * PAGE) as u64)]);
        assert_eq!(unsafe { *dm.app_ptr().add(3 * PAGE) }, 4);
        // "Serwer" zmienił zawartość: kolejne unieważnienie zrzuca stronę z widoku, odczyt ją pobiera.
        r.fill(3 * PAGE, &[99; 16]);
        assert_eq!(unsafe { *dm.app_ptr().add(3 * PAGE) }, 99, "bez unieważnienia aplikacja widzi to, co jest w pamięci podręcznej stron");
        r.invalidate(&[(3 * PAGE as u64, PAGE as u64)]);
        assert_eq!(unsafe { *dm.app_ptr().add(3 * PAGE) }, 4, "po unieważnieniu strona wraca z serwera");
        r.unregister();
    }

    #[test]
    fn fresh_pages_are_mapped_without_a_fetch_and_writes_are_tracked() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dm, r)) = setup(16) else { return };
        let before = FAULTS_FETCHED.load(Ordering::Relaxed);
        r.invalidate(&[(0, PAGE as u64)]);
        r.mark_fresh(0, PAGE);
        r.fill(0, &[7; 8]);
        assert_eq!(unsafe { *dm.app_ptr() }, 7);
        assert_eq!(FAULTS_FETCHED.load(Ordering::Relaxed), before, "świeża strona nie wymaga obiegu");
        // Zapis przez widok aplikacji jest widoczny w widoku własnym.
        unsafe { *dm.app_ptr().add(PAGE + 1) = 5 };
        assert_eq!(unsafe { *dm.own_ptr().add(PAGE + 1) }, 5);
        r.unregister();
    }
    #[test]
    fn sequential_reading_doubles_the_window_and_a_jump_resets_it() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dm, r)) = setup(1024) else { return };
        r.invalidate(&[(0, (1024 * PAGE) as u64)]);
        let before = FAULTS_FETCHED.load(Ordering::Relaxed);
        // Strony 0, 16, 48 (po oknach 16, 32...): sekwencyjnie, więc okna rosną 16 -> 32 -> 64.
        for p in [0usize, 16, 48, 112] {
            assert_eq!(unsafe { *dm.app_ptr().add(p * PAGE) }, (p + 1) as u8);
        }
        let seq = FAULTS_FETCHED.load(Ordering::Relaxed) - before;
        assert_eq!(seq, 4);
        // Odczyt całości po kolei: znacznie mniej pobrań niż stron / 16 (=64).
        let before = FAULTS_FETCHED.load(Ordering::Relaxed);
        for p in 112..1024 {
            let v = unsafe { *dm.app_ptr().add(p * PAGE) };
            assert_eq!(v, (p + 1) as u8, "strona {p}");
        }
        let n = FAULTS_FETCHED.load(Ordering::Relaxed) - before;
        assert!(n < 20, "okno rośnie przy odczycie sekwencyjnym: {n} pobrań");
        r.unregister();
    }

    #[test]
    fn invalidated_pages_are_reported_for_recheck_once() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dm, r)) = setup(16) else { return };
        r.invalidate(&[(0, (16 * PAGE) as u64)]);
        assert!(r.take_recheck().is_empty(), "nic jeszcze nie było odwzorowane w widoku aplikacji, więc nic nie zrzucono");
        assert_eq!(unsafe { *dm.app_ptr().add(2 * PAGE) }, 3); // pobranie okna: strony 0..16 odwzorowane
        r.invalidate(&[(0, (16 * PAGE) as u64)]);
        let v = r.take_recheck();
        assert_eq!(v, vec![(0, 16 * PAGE)], "zrzucone strony (te, które były odwzorowane) czekają na weryfikację");
        assert!(r.take_recheck().is_empty(), "lista jest opróżniana");
        r.unregister();
    }

}
