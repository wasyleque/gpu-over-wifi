//! Śledzenie zapisów do odwzorowanej pamięci przez jądro (Linux 6.7+): userfaultfd w trybie asynchronicznej ochrony zapisu
//! plus `PAGEMAP_SCAN`. Jądro samo zaznacza strony zapisane przez aplikację (także przez `read()` do bufora) i oddaje ich listę
//! jednym wywołaniem, więc zamiast hashować całą pamięć przy każdym `vkQueueSubmit` sprawdzamy tylko strony zmienione.
//! Gdy mechanizm jest niedostępny (stare jądro, kontener), `Tracker::new` zwraca `None` i działa pełne skanowanie skrótów.

use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const SYS_USERFAULTFD: i64 = 323;
const O_CLOEXEC: i64 = 0o2000000;
const O_NONBLOCK: i64 = 0o4000;
const UFFD_USER_MODE_ONLY: i64 = 1;
const UFFD_API: u64 = 0xAA;
const UFFD_FEATURE_WP_UNPOPULATED: u64 = 1 << 13;
const UFFD_FEATURE_WP_ASYNC: u64 = 1 << 15;
const UFFDIO_API: u64 = 0xc018_aa3f;
const UFFDIO_REGISTER: u64 = 0xc020_aa00;
const UFFDIO_WRITEPROTECT: u64 = 0xc018_aa06;
const UFFDIO_UNREGISTER: u64 = 0x8010_aa01;
const UFFDIO_REGISTER_MODE_WP: u64 = 2;
const UFFDIO_WRITEPROTECT_MODE_WP: u64 = 1;
const PAGEMAP_SCAN: u64 = 0xc060_6610;
const PM_SCAN_WP_MATCHING: u64 = 1;
const PM_SCAN_CHECK_WPASYNC: u64 = 2;
const PAGE_IS_WRITTEN: u64 = 2;
pub const PAGE: usize = 4096;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn syscall(num: i64, ...) -> i64;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
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
struct PmScanArg {
    size: u64,
    flags: u64,
    start: u64,
    end: u64,
    walk_end: u64,
    vec: u64,
    vec_len: u64,
    max_pages: u64,
    category_inverted: u64,
    category_mask: u64,
    category_anyof_mask: u64,
    return_mask: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PageRegion {
    start: u64,
    end: u64,
    categories: u64,
}

/// Obszar pamięci (wyrównany do strony) z jądrowym śledzeniem zapisów.
pub struct Tracker {
    _uffd: Option<OwnedFd>,
    pagemap: File,
    base: usize,
    len: usize,
}

impl Tracker {
    /// Rejestruje `[ptr, ptr+len)` (wyrównane do strony) do śledzenia. `None`, gdy jądro tego nie obsługuje.
    pub fn new(ptr: *mut u8, len: usize) -> Option<Tracker> {
        let base = ptr as usize;
        if !base.is_multiple_of(PAGE) || len == 0 || !len.is_multiple_of(PAGE) {
            return None;
        }
        // SAFETY: wywołania systemowe na poprawnych strukturach; deskryptory są zamykane przez `OwnedFd`/`File`.
        unsafe {
            let fd = syscall(SYS_USERFAULTFD, O_CLOEXEC | O_NONBLOCK | UFFD_USER_MODE_ONLY);
            if fd < 0 {
                return None;
            }
            let uffd = OwnedFd::from_raw_fd(fd as i32);
            let mut api = UffdioApi { api: UFFD_API, features: UFFD_FEATURE_WP_ASYNC | UFFD_FEATURE_WP_UNPOPULATED, ioctls: 0 };
            if ioctl(uffd.as_raw_fd(), UFFDIO_API, &mut api as *mut UffdioApi) < 0 {
                return None;
            }
            let mut reg = UffdioRegister { start: base as u64, len: len as u64, mode: UFFDIO_REGISTER_MODE_WP, ioctls: 0 };
            if ioctl(uffd.as_raw_fd(), UFFDIO_REGISTER, &mut reg as *mut UffdioRegister) < 0 {
                return None;
            }
            let mut wp = UffdioWriteProtect { start: base as u64, len: len as u64, mode: UFFDIO_WRITEPROTECT_MODE_WP };
            if ioctl(uffd.as_raw_fd(), UFFDIO_WRITEPROTECT, &mut wp as *mut UffdioWriteProtect) < 0 {
                return None;
            }
            let pagemap = File::open("/proc/self/pagemap").ok()?;
            let t = Tracker { _uffd: Some(uffd), pagemap, base, len };
            // Tuż po włączeniu ochrony wszystkie strony liczą się jako "zapisane": jeden pełny odczyt zeruje stan (i sprawdza,
            // że jądro obsługuje PAGEMAP_SCAN już tutaj, a nie przy pierwszym użyciu).
            t.take_dirty(0, len)?;
            Some(t)
        }
    }

    /// Rejestruje zakres w `userfaultfd`, ale NIE włącza ochrony zapisu: robi to `arm` dla podzakresów (arena obejmuje dziesiątki GiB adresów,
    /// a ochrona całości rozdzielałaby tablice stron dla całego zakresu).
    pub fn new_unarmed(ptr: *mut u8, len: usize) -> Option<Tracker> {
        let base = ptr as usize;
        if !base.is_multiple_of(PAGE) || len == 0 || !len.is_multiple_of(PAGE) {
            return None;
        }
        // SAFETY: wywołania systemowe na poprawnych strukturach; deskryptory są zamykane przez `OwnedFd`/`File`.
        unsafe {
            let fd = syscall(SYS_USERFAULTFD, O_CLOEXEC | O_NONBLOCK | UFFD_USER_MODE_ONLY);
            if fd < 0 {
                return None;
            }
            let uffd = OwnedFd::from_raw_fd(fd as i32);
            let mut api = UffdioApi { api: UFFD_API, features: UFFD_FEATURE_WP_ASYNC | UFFD_FEATURE_WP_UNPOPULATED, ioctls: 0 };
            if ioctl(uffd.as_raw_fd(), UFFDIO_API, &mut api as *mut UffdioApi) < 0 {
                return None;
            }
            let mut reg = UffdioRegister { start: base as u64, len: len as u64, mode: UFFDIO_REGISTER_MODE_WP, ioctls: 0 };
            if ioctl(uffd.as_raw_fd(), UFFDIO_REGISTER, &mut reg as *mut UffdioRegister) < 0 {
                return None;
            }
            let pagemap = File::open("/proc/self/pagemap").ok()?;
            Some(Tracker { _uffd: Some(uffd), pagemap, base, len })
        }
    }

    /// Włącza ochronę zapisu dla `[offset, offset+len)` (wyrównane do strony) i zeruje stan "zapisane" tego podzakresu.
    pub fn arm(&self, offset: usize, len: usize) -> Option<()> {
        let fd = self._uffd.as_ref()?.as_raw_fd();
        let mut wp = UffdioWriteProtect { start: (self.base + offset) as u64, len: len as u64, mode: UFFDIO_WRITEPROTECT_MODE_WP };
        // SAFETY: zakres leży w zarejestrowanym obszarze; struktura żyje przez czas wywołania.
        if unsafe { ioctl(fd, UFFDIO_WRITEPROTECT, &mut wp as *mut UffdioWriteProtect) } < 0 {
            return None;
        }
        self.take_dirty(offset, len)?;
        Some(())
    }

    /// Wyrejestrowuje `[offset, offset+len)` z uffd tego trackera (strony tracą ochronę zapisu); używane przy chłodzeniu bloku.
    pub fn unregister(&self, offset: usize, len: usize) -> bool {
        let Some(fd) = self._uffd.as_ref().map(|f| f.as_raw_fd()) else { return false };
        let mut r = [(self.base + offset) as u64, len as u64];
        // SAFETY: `r` to `struct uffdio_range`; zakres leży w arenie.
        unsafe { ioctl(fd, UFFDIO_UNREGISTER, r.as_mut_ptr()) >= 0 }
    }

    /// Rejestruje ponownie `[offset, offset+len)` w trybie WP i włącza ochronę (jak `arm`).
    pub fn reregister_and_arm(&self, offset: usize, len: usize) -> bool {
        let Some(fd) = self._uffd.as_ref().map(|f| f.as_raw_fd()) else { return false };
        let mut reg = UffdioRegister { start: (self.base + offset) as u64, len: len as u64, mode: UFFDIO_REGISTER_MODE_WP, ioctls: 0 };
        // SAFETY: struktura żyje przez czas wywołania.
        if unsafe { ioctl(fd, UFFDIO_REGISTER, &mut reg as *mut UffdioRegister) } < 0 {
            return false;
        }
        self.arm(offset, len).is_some()
    }

    /// Śledzenie obszaru zarejestrowanego już przez inny userfaultfd z ochroną zapisu (odczyt leniwy): potrzebny tylko `pagemap`.
    pub fn attached(base: *mut u8, len: usize) -> Option<Tracker> {
        let base = base as usize;
        if !base.is_multiple_of(PAGE) || len == 0 || !len.is_multiple_of(PAGE) {
            return None;
        }
        let pagemap = File::open("/proc/self/pagemap").ok()?;
        let t = Tracker { _uffd: None, pagemap, base, len };
        t.take_dirty(0, len)?;
        Some(t)
    }

    /// Zakresy bajtów `[od, do)` względem początku obszaru zapisane od ostatniego wywołania dla tego zakresu; jednocześnie
    /// ponownie włącza ochronę zapisu. `offset` i `len` muszą być wyrównane do strony.
    pub fn take_dirty(&self, offset: usize, len: usize) -> Option<Vec<(usize, usize)>> {
        if !offset.is_multiple_of(PAGE) || !len.is_multiple_of(PAGE) || offset.checked_add(len)? > self.len {
            return None;
        }
        let mut out: Vec<(usize, usize)> = Vec::new();
        let (mut cur, end) = ((self.base + offset) as u64, (self.base + offset + len) as u64);
        let mut regions = [PageRegion::default(); 256];
        while cur < end {
            let mut arg = PmScanArg {
                size: std::mem::size_of::<PmScanArg>() as u64,
                flags: PM_SCAN_WP_MATCHING | PM_SCAN_CHECK_WPASYNC,
                start: cur,
                end,
                walk_end: 0,
                vec: regions.as_mut_ptr() as u64,
                vec_len: regions.len() as u64,
                max_pages: 0,
                category_inverted: 0,
                category_mask: PAGE_IS_WRITTEN,
                category_anyof_mask: 0,
                return_mask: PAGE_IS_WRITTEN,
            };
            // SAFETY: `arg` i tablica regionów żyją przez czas wywołania; jądro wypełnia najwyżej `vec_len` wpisów.
            let t_io = std::time::Instant::now();
            let n = unsafe { ioctl(self.pagemap.as_raw_fd(), PAGEMAP_SCAN, &mut arg as *mut PmScanArg) };
            IOCTL_NS.fetch_add(t_io.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
            IOCTL_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < 0 {
                return None;
            }
            for r in &regions[..n as usize] {
                let (a, b) = (r.start as usize - self.base, r.end as usize - self.base);
                match out.last_mut() {
                    Some(last) if last.1 == a => last.1 = b,
                    _ => out.push((a, b)),
                }
            }
            if arg.walk_end <= cur {
                break;
            }
            cur = arg.walk_end;
        }
        Some(out)
    }
}

/// Najmniejszy obszar (bajty), od którego opłaca się skan równoległy (wybudzenie wątku kosztuje dziesiątki µs, skan 1 MiB ok. 1,5 µs·256).
const PAR_MIN_BYTES: usize = 64 << 20;

/// Liczba wątków skanu (`GPU_ICD_SCAN_THREADS`, domyślnie 1 = skan w wątku wołającym).
fn scan_threads() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("GPU_ICD_SCAN_THREADS").ok().and_then(|v| v.parse::<usize>().ok()).map_or(1, |n| n.clamp(1, 16)))
}

/// Rozmiar kawałka skanu równoległego: drobne kawałki wyrównują obciążenie (koszt zależy od stron faktycznie zapełnionych, nie od bajtów).
const PAR_CHUNK: usize = 16 << 20;

/// Dzieli przedziały `(offset, długość)` na kawałki po najwyżej `chunk` bajtów (wyrównane do strony), zachowując kolejność.
fn split_extents(extents: &[(usize, usize)], chunk: usize) -> Vec<(usize, usize)> {
    let chunk = chunk.div_ceil(PAGE).max(1) * PAGE;
    let mut v = Vec::new();
    for &(mut o, mut l) in extents {
        while l > 0 {
            let take = l.min(chunk);
            v.push((o, take));
            o += take;
            l -= take;
        }
    }
    v
}

type ScanReply = (usize, Option<Vec<(usize, usize)>>);
type ScanJob = (usize, (usize, usize), std::sync::mpsc::Sender<ScanReply>);

/// Kolejka kawałków skanu: `Condvar` zwalnia mutex na czas oczekiwania (wątek gry pobiera z niej bez blokowania się na bezczynnych wątkach roboczych).
struct JobQueue {
    q: std::sync::Mutex<std::collections::VecDeque<ScanJob>>,
    cv: std::sync::Condvar,
}

impl JobQueue {
    fn push_all(&self, jobs: Vec<ScanJob>) {
        self.q.lock().unwrap_or_else(|e| e.into_inner()).extend(jobs);
        self.cv.notify_all();
    }
    fn try_pop(&self) -> Option<ScanJob> {
        self.q.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
    }
    fn wait_pop(&self) -> ScanJob {
        let mut g = self.q.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(j) = g.pop_front() {
                return j;
            }
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }
}

fn run_job(tracker: &Tracker, job: ScanJob) {
    let (idx, (o, l), reply) = job;
    let _ = reply.send((idx, tracker.take_dirty(o, l)));
}

/// Wątki robocze skanu (tworzone przy pierwszym użyciu) i wspólna kolejka kawałków; działają na `'static` trackerze areny.
fn pool(tracker: &'static Tracker, n: usize) -> &'static JobQueue {
    static P: std::sync::OnceLock<&'static JobQueue> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let q: &'static JobQueue = Box::leak(Box::new(JobQueue { q: Default::default(), cv: Default::default() }));
        for i in 0..n.saturating_sub(1) {
            let _ = std::thread::Builder::new().name(format!("icd-scan-{i}")).spawn(move || loop {
                run_job(tracker, q.wait_pop());
            });
        }
        q
    })
}

fn par_scan(tracker: &Tracker, extents: &[(usize, usize)], threads: usize) -> Option<Vec<(usize, usize)>> {
    let chunks = split_extents(extents, PAR_CHUNK);
    // SAFETY: tracker należy do areny (`'static`, nigdy nie jest zwalniana); wątki robocze używają go tylko do `take_dirty(&self)`.
    let tracker_static: &'static Tracker = unsafe { &*(tracker as *const Tracker) };
    let queue = pool(tracker_static, threads);
    let (rtx, rrx) = std::sync::mpsc::channel();
    let n = chunks.len();
    queue.push_all(chunks.into_iter().enumerate().map(|(i, c)| (i, c, rtx.clone())).collect());
    drop(rtx);
    // Wątek wołający też bierze kawałki z kolejki, dopóki jakieś są.
    while let Some(j) = queue.try_pop() {
        run_job(tracker, j);
    }
    let mut parts: Vec<Option<Vec<(usize, usize)>>> = vec![None; n];
    for _ in 0..n {
        let (idx, r) = rrx.recv().ok()?;
        parts[idx] = Some(r?);
    }
    Some(parts.into_iter().flatten().flatten().collect())
}

/// Jedna wspólna arena adresów dla kopii wszystkich odwzorowanych pamięci: jeden zarejestrowany obszar, więc ślad zapisów całej gry pobieramy jednym
/// (lub kilkoma przy dziurach) wywołaniem `PAGEMAP_SCAN` zamiast wywołania na każdą pamięć (w Cyberpunku ponad 200 skanów na klatkę po ok. 60 µs,
/// w tym haszowanie małych pamięci bez śledzenia jądrowego).
pub struct Arena {
    base: usize,
    tracker: Tracker,
    st: std::sync::Mutex<ArenaState>,
    hc: Option<HotCold>,
}

/// Rozmiar bloku gorący/zimny (1 MiB): pomiar lokalności zapisów w Cyberpunku (59 bloków w >=1% skanów, zbiór gorący 39 bloków na 100 skanów).
const BLOCK: usize = 1 << 20;

/// Ustawienia śledzenia gorący/zimny (`GPU_ICD_HOTCOLD=1`): blok bez zapisu przez `cool_after` skanów jest chłodzony (przechodzi do pułapki synchronicznej),
/// kandydatów szukamy co `period` skanów.
#[derive(Clone, Copy)]
pub struct HcConfig {
    pub cool_after: u64,
    pub period: u64,
}

struct HotCold {
    trap: crate::coldtrap::ColdTrap,
    m: std::sync::Mutex<HcState>,
    period: u64,
    scans: std::sync::atomic::AtomicU64,
    /// Ustawiane przy nieudanym przenoszeniu zakresu między uffd: od tej chwili skany zwracają `None` (wołający wraca do haszowania stron).
    broken: std::sync::atomic::AtomicBool,
}

struct HcState {
    hot: crate::hotset::HotSet,
    cold: std::collections::BTreeSet<usize>,
}

/// Liczniki śledzenia gorący/zimny.
pub static HC_COOLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static HC_HEATS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static HC_FAULT_PAGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static HC_SCANNED_PAGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static HC_COLD_BLOCKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn hotcold_report() -> Option<String> {
    use std::sync::atomic::Ordering::Relaxed;
    let c = HC_COOLS.load(Relaxed);
    let h = HC_HEATS.load(Relaxed);
    (c + h + HC_SCANNED_PAGES.load(Relaxed) > 0).then(|| {
        format!(
            "gorący/zimny: ochłodzeń {c}, ogrzań {h}, strony z pułapki {}, zimnych bloków teraz {}, skanowane strony łącznie {} MiB",
            HC_FAULT_PAGES.load(Relaxed),
            HC_COLD_BLOCKS.load(Relaxed),
            HC_SCANNED_PAGES.load(Relaxed) * PAGE as u64 / 1048576
        )
    })
}

/// Sortuje i scala zakresy `[od, do)` (nakładające się i stykające się).
fn merge_ranges(v: &mut Vec<(usize, usize)>) {
    v.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(v.len());
    for &(a, b) in v.iter() {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    *v = out;
}

struct ArenaState {
    /// Wolne zakresy (offset -> długość) i zajęte (offset -> długość).
    free: std::collections::BTreeMap<usize, usize>,
    used: std::collections::BTreeMap<usize, usize>,
}

/// Statystyka per przydział areny (`GPU_ICD_SCANSTAT=1`): offset -> (długość, skany z zapisem, strony zapisane). Do oceny, jaka część skanowanego obszaru bywa w ogóle zapisywana.
static ALLOC_HITS: std::sync::Mutex<Option<std::collections::HashMap<usize, (usize, u64, u64)>>> = std::sync::Mutex::new(None);

/// Statystyka bloków 256 stron (1 MiB) w arenie: blok -> (skany z zapisem, numer ostatniego skanu z zapisem); plus sumy rozmiaru zbioru gorącego w oknach 20/100/500 skanów.
static BLOCK_HITS: std::sync::Mutex<Option<BlockStat>> = std::sync::Mutex::new(None);
#[derive(Default)]
struct BlockStat {
    blocks: std::collections::HashMap<usize, (u64, u64)>,
    scans: u64,
    per_scan_blocks: u64,
    window_sum: [u64; 3],
    window_n: u64,
}
const BLOCK_PAGES: usize = 256;
const WINDOWS: [u64; 3] = [20, 100, 500];

fn scanstat_blocks(out: &[(usize, usize)]) {
    let mut g = BLOCK_HITS.lock().unwrap_or_else(|e| e.into_inner());
    let b = g.get_or_insert_with(Default::default);
    b.scans += 1;
    let n = b.scans;
    let mut seen: std::collections::HashSet<usize> = Default::default();
    for &(x, y) in out {
        let (f, l) = (x / PAGE / BLOCK_PAGES, (y - 1) / PAGE / BLOCK_PAGES);
        for blk in f..=l {
            if seen.insert(blk) {
                let e = b.blocks.entry(blk).or_insert((0, 0));
                e.0 += 1;
                e.1 = n;
            }
        }
    }
    b.per_scan_blocks += seen.len() as u64;
    if n % 16 == 0 {
        for (i, w) in WINDOWS.iter().enumerate() {
            b.window_sum[i] += b.blocks.values().filter(|v| v.1 + w > n).count() as u64;
        }
        b.window_n += 1;
    }
}

pub fn block_report() -> Option<String> {
    let g = BLOCK_HITS.lock().unwrap_or_else(|e| e.into_inner());
    let b = g.as_ref()?;
    let wn = b.window_n.max(1);
    let ever = b.blocks.len();
    let hot1 = b.blocks.values().filter(|v| v.0 * 100 >= b.scans).count();
    Some(format!(
        "bloki 1 MiB po {} skanach: zapisane kiedykolwiek {ever}, w >=1% skanów {hot1}; średnio {:.1} bloków zapisanych na skan; zbiór gorący (bloki zapisane w ostatnich 20/100/500 skanach) średnio {:.0}/{:.0}/{:.0} bloków",
        b.scans, b.per_scan_blocks as f64 / b.scans.max(1) as f64,
        b.window_sum[0] as f64 / wn as f64, b.window_sum[1] as f64 / wn as f64, b.window_sum[2] as f64 / wn as f64
    ))
}

fn scanstat_on() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("GPU_ICD_SCANSTAT").is_some())
}

/// Raport: ile przydziałów i MiB w arenie, ile z nich kiedykolwiek miało zapis, rozkład "gorących" i "zimnych" (zapis w mniej niż 1% skanów).
pub fn scan_report() -> Option<String> {
    let calls = SCAN_CALLS.load(std::sync::atomic::Ordering::Relaxed).max(1);
    let g = ALLOC_HITS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.as_ref()?;
    let mib = |b: usize| b as f64 / 1048576.0;
    let (mut hot_b, mut cold_b, mut never_b) = (0usize, 0usize, 0usize);
    let (mut hot_n, mut cold_n, mut never_n) = (0, 0, 0);
    for &(len, scans, _) in m.values() {
        if scans == 0 { never_b += len; never_n += 1; } else if scans * 100 >= calls { hot_b += len; hot_n += 1; } else { cold_b += len; cold_n += 1; }
    }
    let mut top: Vec<_> = m.iter().map(|(o, v)| (*o, *v)).collect();
    top.sort_by_key(|(_, v)| std::cmp::Reverse(v.1));
    let tops: Vec<String> = top.iter().take(8).map(|(o, (l, sc, pg))| format!("0x{o:x} ({:.0} MiB) skany z zapisem {sc}/{calls}, stron {pg}", mib(*l))).collect();
    Some(format!(
        "przydziały areny po skanach ({calls}): gorące (zapis w >=1% skanów) {hot_n} szt. {:.0} MiB; zimne (<1%) {cold_n} szt. {:.0} MiB; nigdy {never_n} szt. {:.0} MiB; najczęściej zapisywane: {}",
        mib(hot_b), mib(cold_b), mib(never_b), tops.join("; ")
    ))
}

/// Rozbicie czasu skanu: sam `ioctl(PAGEMAP_SCAN)` (ns i liczba wywołań, w tym kolejne porcje przy dziurach).
pub static IOCTL_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static IOCTL_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Diagnostyka skanów zbiorczych: wywołania, skanowane przedziały i strony, zwrócone zakresy i strony zapisane.
pub static SCAN_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SCAN_EXTENTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SCAN_PAGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SCAN_RANGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SCAN_DIRTY_PAGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static ARENA: std::sync::OnceLock<Option<Arena>> = std::sync::OnceLock::new();

/// Arena procesu (tworzona przy pierwszym użyciu); `None`, gdy jądro lub ustawienia (`GPU_ICD_NO_MEMTRACK`) jej nie dopuszczają.
pub fn arena() -> Option<&'static Arena> {
    ARENA.get_or_init(Arena::create).as_ref()
}

impl Arena {
    fn create() -> Option<Arena> {
        if std::env::var("GPU_ICD_NO_MEMTRACK").is_ok() {
            return None;
        }
        let cfg = std::env::var_os("GPU_ICD_HOTCOLD").map(|_| HcConfig {
            cool_after: std::env::var("GPU_ICD_HOTCOLD_AFTER").ok().and_then(|v| v.parse().ok()).unwrap_or(400),
            period: 32,
        });
        for len in [64usize << 30, 16 << 30, 4 << 30] {
            if let Some(a) = Arena::with_len(len, cfg) {
                return Some(a);
            }
        }
        None
    }

    /// Arena o podanym rozmiarze; `hc` włącza śledzenie gorący/zimny (wymaga uffd w trybie jądrowym, inaczej jest po cichu wyłączone).
    pub fn with_len(len: usize, hc: Option<HcConfig>) -> Option<Arena> {
        // PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE: adresy bez rezerwacji pamięci, strony powstają przy pierwszym zapisie.
        // SAFETY: anonimowe odwzorowanie bez adresu docelowego.
        let ptr = unsafe { mmap(std::ptr::null_mut(), len, 3, 0x22 | 0x4000, -1, 0) };
        if ptr as isize == -1 {
            return None;
        }
        let Some(tracker) = Tracker::new_unarmed(ptr, len) else {
            // SAFETY: zwalniamy odwzorowanie utworzone powyżej.
            unsafe { munmap(ptr, len) };
            return None;
        };
        let mut free = std::collections::BTreeMap::new();
        free.insert(0, len);
        let hc = hc.and_then(|cfg| {
            let trap = crate::coldtrap::ColdTrap::new();
            if trap.is_none() {
                eprintln!("gpu-icd: GPU_ICD_HOTCOLD: uffd w trybie jądrowym niedostępny (vm.unprivileged_userfaultfd=0?): śledzenie gorący/zimny wyłączone");
            }
            trap.map(|trap| {
                let mut hot = crate::hotset::HotSet::new(len.div_ceil(BLOCK), cfg.cool_after);
                hot.set_cool_after(cfg.cool_after);
                HotCold { trap, m: std::sync::Mutex::new(HcState { hot, cold: Default::default() }), period: cfg.period.max(1), scans: Default::default(), broken: Default::default() }
            })
        });
        Some(Arena { base: ptr as usize, tracker, st: std::sync::Mutex::new(ArenaState { free, used: Default::default() }), hc })
    }

    /// Przydziela `len` bajtów (zaokrąglone do strony) i włącza ich śledzenie; zwraca offset w arenie.
    pub fn alloc(&self, len: usize) -> Option<usize> {
        let len = len.max(1).div_ceil(PAGE) * PAGE;
        let mut g = self.st.lock().unwrap_or_else(|e| e.into_inner());
        let (&off, &flen) = g.free.iter().find(|(_, l)| **l >= len)?;
        g.free.remove(&off);
        if flen > len {
            g.free.insert(off + len, flen - len);
        }
        g.used.insert(off, len);
        drop(g);
        if self.tracker.arm(off, len).is_none() {
            self.free(off, len);
            return None;
        }
        if let Some(hc) = &self.hc {
            let scan = hc.scans.load(std::sync::atomic::Ordering::Relaxed);
            let mut s = hc.m.lock().unwrap_or_else(|e| e.into_inner());
            for b in off / BLOCK..=(off + len - 1) / BLOCK {
                s.hot.reset(b, scan);
            }
        }
        Some(off)
    }

    /// Zwalnia zakres (strony wracają do systemu) i skleja sąsiednie wolne.
    pub fn free(&self, off: usize, len: usize) {
        if let Some(hc) = &self.hc {
            // Zimne bloki leżą w całości w przydziale: przed zwolnieniem wracają do trybu asynchronicznego (nowy przydział dostaje świeżą ochronę przez `arm`).
            let blocks: Vec<usize> = hc.m.lock().unwrap_or_else(|e| e.into_inner()).cold.range(off / BLOCK..=(off + len.max(1) - 1) / BLOCK).copied().collect();
            for b in blocks {
                let mut sink = Vec::new();
                self.heat_block(hc, b, &mut sink);
            }
        }
        // SAFETY: zakres należał do tego przydziału; `MADV_DONTNEED` na pamięci anonimowej zeruje i zwalnia strony.
        unsafe { madvise((self.base + off) as *mut u8, len, 4) };
        let mut g = self.st.lock().unwrap_or_else(|e| e.into_inner());
        g.used.remove(&off);
        let (mut start, mut size) = (off, len);
        if let Some((&pa, &pl)) = g.free.range(..off).next_back() {
            if pa + pl == off {
                g.free.remove(&pa);
                start = pa;
                size += pl;
            }
        }
        if let Some(&nl) = g.free.get(&(off + len)) {
            g.free.remove(&(off + len));
            size += nl;
        }
        g.free.insert(start, size);
    }

    pub fn ptr(&self, off: usize) -> *mut u8 {
        (self.base + off) as *mut u8
    }

    /// Zakresy `[od, do)` (offsety w arenie) zapisane od ostatniego skanu we WSZYSTKICH przydziałach; zeruje ich stan. Skanuje tylko zajęte przedziały
    /// (sąsiednie przydziały scalone), więc koszt zależy od liczby zapisanych stron, nie od rozmiaru areny.
    pub fn take_dirty_all(&self) -> Option<Vec<(usize, usize)>> {
        let extents: Vec<(usize, usize)> = {
            let g = self.st.lock().unwrap_or_else(|e| e.into_inner());
            let mut v: Vec<(usize, usize)> = Vec::new();
            for (&o, &l) in &g.used {
                match v.last_mut() {
                    Some(last) if last.0 + last.1 == o => last.1 += l,
                    _ => v.push((o, l)),
                }
            }
            v
        };
        let mut out: Vec<(usize, usize)> = Vec::new();
        if let Some(hc) = self.hc.as_ref().filter(|h| !h.broken.load(std::sync::atomic::Ordering::Relaxed)) {
            for &(o, l) in &extents {
                self.scan_range(hc, o, l, &mut out)?;
            }
            self.cooling_pass(hc, &mut out);
            merge_ranges(&mut out);
            return self.finish_scan(out, extents.len());
        }
        let threads = scan_threads();
        let total: usize = extents.iter().map(|e| e.1).sum();
        if threads > 1 && total >= PAR_MIN_BYTES {
            // Skan równoległy: rozłączne kawałki areny skanują wątki robocze i bieżący; wynik składamy w kolejności offsetów.
            for &(_, l) in &extents {
                SCAN_PAGES.fetch_add((l / PAGE) as u64, std::sync::atomic::Ordering::Relaxed);
            }
            out = par_scan(&self.tracker, &extents, threads)?;
        } else {
            for &(o, l) in &extents {
                SCAN_PAGES.fetch_add((l / PAGE) as u64, std::sync::atomic::Ordering::Relaxed);
                out.extend(self.tracker.take_dirty(o, l)?);
            }
        }
        self.finish_scan(out, extents.len())
    }

    fn finish_scan(&self, out: Vec<(usize, usize)>, n_extents: usize) -> Option<Vec<(usize, usize)>> {
        SCAN_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        SCAN_EXTENTS.fetch_add(n_extents as u64, std::sync::atomic::Ordering::Relaxed);
        SCAN_RANGES.fetch_add(out.len() as u64, std::sync::atomic::Ordering::Relaxed);
        SCAN_DIRTY_PAGES.fetch_add(out.iter().map(|r| ((r.1 - r.0) / PAGE) as u64).sum::<u64>(), std::sync::atomic::Ordering::Relaxed);
        if scanstat_on() {
            scanstat_blocks(&out);
            let g = self.st.lock().unwrap_or_else(|e| e.into_inner());
            let mut h = ALLOC_HITS.lock().unwrap_or_else(|e| e.into_inner());
            let m = h.get_or_insert_with(Default::default);
            for (&o, &l) in &g.used {
                m.entry(o).or_insert((l, 0, 0)).0 = l;
            }
            let mut seen: std::collections::HashSet<usize> = Default::default();
            for &(a, b) in &out {
                if let Some((&o, &l)) = g.used.range(..=a).next_back() {
                    if a < o + l {
                        let e = m.entry(o).or_insert((l, 0, 0));
                        e.2 += ((b - a) / PAGE) as u64;
                        if seen.insert(o) {
                            e.1 += 1;
                        }
                    }
                }
            }
        }
        Some(out)
    }

    /// Jak `take_dirty_all`, ale tylko dla jednego przydziału (`Unmap` jednej pamięci); zakresy względem początku przydziału.
    pub fn take_dirty_range(&self, off: usize, len: usize) -> Option<Vec<(usize, usize)>> {
        if let Some(hc) = self.hc.as_ref().filter(|h| !h.broken.load(std::sync::atomic::Ordering::Relaxed)) {
            let mut out = Vec::new();
            self.scan_range(hc, off, len, &mut out)?;
            merge_ranges(&mut out);
            return Some(out.into_iter().filter_map(|(a, b)| (a.max(off) < b.min(off + len)).then(|| (a.max(off) - off, b.min(off + len) - off))).collect());
        }
        self.tracker.take_dirty(off, len).map(|v| v.into_iter().map(|(a, b)| (a - off, b - off)).collect())
    }

    /// Skan `[off, off+len)` z uwzględnieniem zimnych bloków: gorące części skanuje `PAGEMAP_SCAN`, zimne zapisy pochodzą z listy pułapki synchronicznej,
    /// a blok z zapisem wraca do trybu gorącego (cały liczy się jako brudny: strony zapisane w oknie przenoszenia odsieje porównanie skrótów).
    fn scan_range(&self, hc: &HotCold, off: usize, len: usize, out: &mut Vec<(usize, usize)>) -> Option<()> {
        use std::sync::atomic::Ordering::Relaxed;
        if len == 0 {
            return Some(());
        }
        let end = off + len;
        let cold: Vec<usize> = hc.m.lock().unwrap_or_else(|e| e.into_inner()).cold.range(off / BLOCK..=(end - 1) / BLOCK).copied().collect();
        // Zimny blok tylko częściowo w zakresie nie powinien się zdarzyć (zimne leżą w całości w jednym przydziale); na wszelki wypadek ogrzewamy go.
        let (inside, partial): (Vec<usize>, Vec<usize>) = cold.iter().partition(|&&b| b * BLOCK >= off && (b + 1) * BLOCK <= end);
        for b in partial {
            self.heat_block(hc, b, out);
        }
        let mut cur = off;
        let scan = |a: usize, b: usize, out: &mut Vec<(usize, usize)>| -> Option<()> {
            HC_SCANNED_PAGES.fetch_add(((b - a) / PAGE) as u64, Relaxed);
            out.extend(self.tracker.take_dirty(a, b - a)?);
            Some(())
        };
        for &b in &inside {
            let (bs, be) = (b * BLOCK, (b + 1) * BLOCK);
            if cur < bs {
                scan(cur, bs, out)?;
            }
            cur = be;
        }
        if cur < end {
            scan(cur, end, out)?;
        }
        // Zapisy w zimnych blokach: lista pułapki (czytana PO skanie, ale przed odczytem danych przez wołającego).
        let pages = hc.trap.take_pending_in(self.base + off, self.base + end);
        let mut promote: std::collections::BTreeSet<usize> = Default::default();
        for addr in pages {
            let p = addr - self.base;
            HC_FAULT_PAGES.fetch_add(1, Relaxed);
            out.push((p, p + PAGE));
            promote.insert(p / BLOCK);
        }
        // Bloki zapisane w tym skanie są gorące od teraz.
        let scan_no = hc.scans.load(Relaxed);
        {
            let mut s = hc.m.lock().unwrap_or_else(|e| e.into_inner());
            for &(a, b) in out.iter() {
                for blk in a / BLOCK..=(b.max(a + 1) - 1) / BLOCK {
                    s.hot.note_written(scan_no, [blk]);
                }
            }
        }
        for b in promote {
            let is_cold = hc.m.lock().unwrap_or_else(|e| e.into_inner()).cold.contains(&b);
            if is_cold {
                self.heat_block(hc, b, out);
            }
        }
        if hc.broken.load(Relaxed) {
            return None;
        }
        Some(())
    }

    /// Ogrzewa blok: zdejmuje pułapkę, wraca do trybu asynchronicznego z ochroną zapisu. Cały blok trafia na listę brudnych (zawartość czytana PO uzbrojeniu).
    fn heat_block(&self, hc: &HotCold, b: usize, out: &mut Vec<(usize, usize)>) {
        use std::sync::atomic::Ordering::Relaxed;
        let (off, len) = (b * BLOCK, BLOCK);
        let was_cold = hc.m.lock().unwrap_or_else(|e| e.into_inner()).cold.remove(&b);
        if !was_cold {
            return;
        }
        HC_COLD_BLOCKS.fetch_sub(1, Relaxed);
        if !hc.trap.release(self.base + off, len) || !self.tracker.reregister_and_arm(off, len) {
            eprintln!("gpu-icd: HOTCOLD: ogrzanie bloku {b} nie powiodło się: wracamy do pełnego śledzenia skrótami");
            hc.broken.store(true, Relaxed);
        }
        HC_HEATS.fetch_add(1, Relaxed);
        let scan_no = hc.scans.load(Relaxed);
        hc.m.lock().unwrap_or_else(|e| e.into_inner()).hot.reset(b, scan_no);
        out.push((off, off + len));
    }

    /// Ochładza blok: zdejmuje śledzenie asynchroniczne i zakłada pułapkę synchroniczną. Cały blok trafia na listę brudnych (zapisy z okna między skanem
    /// a uzbrojeniem pułapki; porównanie skrótów u wołającego odsieje strony bez zmian).
    fn cool_block(&self, hc: &HotCold, b: usize, out: &mut Vec<(usize, usize)>) {
        use std::sync::atomic::Ordering::Relaxed;
        let (off, len) = (b * BLOCK, BLOCK);
        if !self.tracker.unregister(off, len) {
            return;
        }
        if !hc.trap.protect(self.base + off, len) {
            if !self.tracker.reregister_and_arm(off, len) {
                eprintln!("gpu-icd: HOTCOLD: nie udało się przywrócić śledzenia bloku {b}");
                hc.broken.store(true, Relaxed);
            }
            out.push((off, off + len));
            return;
        }
        hc.m.lock().unwrap_or_else(|e| e.into_inner()).cold.insert(b);
        HC_COLD_BLOCKS.fetch_add(1, Relaxed);
        HC_COOLS.fetch_add(1, Relaxed);
        out.push((off, off + len));
    }

    /// Co `period` skanów: ochładza gorące bloki bez zapisu od `cool_after` skanów, o ile leżą w całości w jednym przydziale.
    fn cooling_pass(&self, hc: &HotCold, out: &mut Vec<(usize, usize)>) {
        use std::sync::atomic::Ordering::Relaxed;
        let scan = hc.scans.fetch_add(1, Relaxed) + 1;
        if scan % hc.period != 0 {
            return;
        }
        let cands = hc.m.lock().unwrap_or_else(|e| e.into_inner()).hot.candidates(scan);
        if cands.is_empty() {
            return;
        }
        let eligible: Vec<usize> = {
            let g = self.st.lock().unwrap_or_else(|e| e.into_inner());
            let cold = hc.m.lock().unwrap_or_else(|e| e.into_inner());
            cands
                .into_iter()
                .filter(|b| !cold.cold.contains(b))
                .filter(|&b| g.used.range(..=b * BLOCK).next_back().is_some_and(|(&o, &l)| o <= b * BLOCK && (b + 1) * BLOCK <= o + l))
                .take(16)
                .collect()
        };
        for b in eligible {
            self.cool_block(hc, b, out);
        }
    }
}

/// Bufor wyrównany do strony, wyzerowany (świeże strony od jądra), o rozmiarze zaokrąglonym w górę do strony.
pub struct PageAlignedBuf {
    ptr: *mut u8,
    len: usize,
    cap: usize,
    /// Podwójne odwzorowanie `memfd` (odczyt leniwy): `ptr` to widok własny, widok aplikacji zwraca `app_ptr`.
    shared: Option<crate::shadow::DoubleMap>,
    /// Offset w arenie (`Arena`), gdy bufor jest jej fragmentem: zwalniany do areny zamiast `munmap`.
    arena_off: Option<usize>,
}

// SAFETY: bufor jest własnością jednej struktury; dostęp współbieżny kontroluje właściciel.
unsafe impl Send for PageAlignedBuf {}

impl PageAlignedBuf {
    pub fn new(len: usize) -> PageAlignedBuf {
        let cap = len.max(1).div_ceil(PAGE) * PAGE;
        // PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS: świeże, wyzerowane strony przydzielane dopiero przy pierwszym zapisie.
        // SAFETY: anonimowe odwzorowanie bez adresu docelowego.
        let ptr = unsafe { mmap(std::ptr::null_mut(), cap, 3, 0x22, -1, 0) };
        assert!(ptr as isize != -1, "brak pamięci na kopię odwzorowanej pamięci");
        PageAlignedBuf { ptr, len, cap, shared: None, arena_off: None }
    }

    /// Bufor w arenie wspólnego śledzenia zapisów; `None`, gdy areny nie ma albo jest pełna.
    pub fn new_in_arena(len: usize) -> Option<PageAlignedBuf> {
        let a = arena()?;
        let cap = len.max(1).div_ceil(PAGE) * PAGE;
        let off = a.alloc(cap)?;
        Some(PageAlignedBuf { ptr: a.ptr(off), len, cap, shared: None, arena_off: Some(off) })
    }

    /// Offset bufora w arenie (do mapowania wyników skanu zbiorczego na pamięci).
    pub fn arena_offset(&self) -> Option<usize> {
        self.arena_off
    }

    /// Bufor na pamięci współdzielonej z dwoma widokami (patrz `lazymap`); strony są od razu w pamięci podręcznej stron, bo odczyt
    /// leniwy opiera się na błędach MINOR, które jądro zgłasza tylko dla stron już obecnych.
    pub fn new_shared(len: usize) -> Option<PageAlignedBuf> {
        let dm = crate::shadow::DoubleMap::new(len.max(1)).ok()?;
        let cap = dm.len();
        let ptr = dm.own_ptr();
        for p in 0..cap / PAGE {
            // SAFETY: zapis zera w granicach widoku własnego (strona nowego `memfd` ma już wartość zero).
            unsafe { std::ptr::write_volatile(ptr.add(p * PAGE), 0) };
        }
        Some(PageAlignedBuf { ptr, len, cap, shared: Some(dm), arena_off: None })
    }

    /// Widok przekazywany aplikacji (dla zwykłego bufora ten sam co `as_ptr`).
    pub fn app_ptr(&self) -> *mut u8 {
        self.shared.as_ref().map_or(self.ptr, |d| d.app_ptr())
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn capacity(&self) -> usize {
        self.cap
    }
}

impl std::ops::Deref for PageAlignedBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` wskazuje na `cap >= len` zainicjowanych bajtów.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl std::ops::DerefMut for PageAlignedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: jak wyżej, dostęp wyłączny przez `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for PageAlignedBuf {
    fn drop(&mut self) {
        if self.shared.is_some() {
            return; // `DoubleMap` zwalnia oba widoki
        }
        if let (Some(off), Some(a)) = (self.arena_off, arena()) {
            a.free(off, self.cap);
            return;
        }
        // SAFETY: zwalniamy odwzorowanie utworzone w `new` z tym samym rozmiarem.
        unsafe { munmap(self.ptr, self.cap) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_reports_exactly_the_pages_the_program_wrote() {
        let mut buf = PageAlignedBuf::new(PAGE * 64);
        let Some(t) = Tracker::new(buf.as_ptr(), buf.capacity()) else {
            eprintln!("POMINIĘTO: jądro bez userfaultfd WP async / PAGEMAP_SCAN");
            return;
        };
        assert!(t.take_dirty(0, PAGE * 64).unwrap().is_empty(), "świeży obszar nie ma zapisanych stron");
        buf[PAGE * 3 + 7] = 1;
        buf[PAGE * 4] = 2;
        buf[PAGE * 40 + 100] = 3;
        let d = t.take_dirty(0, PAGE * 64).unwrap();
        assert_eq!(d, vec![(PAGE * 3, PAGE * 5), (PAGE * 40, PAGE * 41)]);
        assert!(t.take_dirty(0, PAGE * 64).unwrap().is_empty(), "po odczycie lista jest czysta");
        buf[PAGE * 3] = 9;
        assert_eq!(t.take_dirty(0, PAGE * 64).unwrap(), vec![(PAGE * 3, PAGE * 4)]);
    }

    #[test]
    fn writes_made_by_the_kernel_through_read_are_tracked_too() {
        let mut buf = PageAlignedBuf::new(PAGE * 16);
        let Some(t) = Tracker::new(buf.as_ptr(), buf.capacity()) else { return };
        t.take_dirty(0, PAGE * 16).unwrap();
        let mut f = File::open("/dev/zero").unwrap();
        use std::io::Read;
        // read() do strony 5 i 6 wykonuje zapis po stronie jądra: bez ochrony asynchronicznej skończyłby się błędem EFAULT.
        f.read_exact(&mut buf[PAGE * 5..PAGE * 7]).unwrap();
        assert_eq!(t.take_dirty(0, PAGE * 16).unwrap(), vec![(PAGE * 5, PAGE * 7)]);
    }

    #[test]
    fn misaligned_requests_are_refused() {
        let buf = PageAlignedBuf::new(PAGE * 4);
        assert!(Tracker::new(unsafe { buf.as_ptr().add(8) }, PAGE * 3).is_none());
        if let Some(t) = Tracker::new(buf.as_ptr(), buf.capacity()) {
            assert!(t.take_dirty(8, PAGE).is_none());
            assert!(t.take_dirty(0, PAGE * 5).is_none());
        }
    }
    /// Pomiar (ręczny): ile kosztuje jedno `take_dirty` bez zapisów dla jednego dużego obszaru i dla wielu mniejszych.
    #[test]
    #[ignore = "mikropomiar; uruchamiać ręcznie z --nocapture"]
    fn timing_of_take_dirty_by_region_size_and_count() {
        let populate = |b: &mut PageAlignedBuf| {
            for p in (0..b.capacity()).step_by(PAGE) {
                b[p] = 1;
            }
        };
        let measure = |regions: &[(Tracker, usize)]| {
            let n = 400;
            let t0 = std::time::Instant::now();
            for _ in 0..n {
                for (t, len) in regions {
                    let _ = t.take_dirty(0, *len);
                }
            }
            t0.elapsed().as_micros() as f64 / n as f64
        };
        let mut one = PageAlignedBuf::new(320 << 20);
        populate(&mut one);
        let Some(t) = Tracker::new(one.as_ptr(), one.capacity()) else { return };
        let len = one.capacity();
        t.take_dirty(0, len);
        println!("TIMING jeden obszar 320 MB w pełni zapełniony: {:.0} us na przejście", measure(&[(t, len)]));
        let mut bufs: Vec<PageAlignedBuf> = (0..20).map(|_| PageAlignedBuf::new(16 << 20)).collect();
        let mut trackers = Vec::new();
        for b in bufs.iter_mut() {
            populate(b);
            let t = Tracker::new(b.as_ptr(), b.capacity()).unwrap();
            let l = b.capacity();
            t.take_dirty(0, l);
            trackers.push((t, l));
        }
        println!("TIMING 20 obszarów po 16 MB (razem 320 MB): {:.0} us na przejście", measure(&trackers));
        let mut empty = PageAlignedBuf::new(320 << 20);
        let Some(t) = Tracker::new(empty.as_ptr(), empty.capacity()) else { return };
        let l = empty.capacity();
        t.take_dirty(0, l);
        let _ = &mut empty;
        println!("TIMING jeden obszar 320 MB niezapełniony: {:.0} us na przejście", measure(&[(t, l)]));
        let mut small: Vec<PageAlignedBuf> = (0..20).map(|_| PageAlignedBuf::new(1 << 20)).collect();
        let mut st = Vec::new();
        for b in small.iter_mut() {
            populate(b);
            let t = Tracker::new(b.as_ptr(), b.capacity()).unwrap();
            let l = b.capacity();
            t.take_dirty(0, l);
            st.push((t, l));
        }
        println!("TIMING 20 obszarów po 1 MB: {:.0} us na przejście", measure(&st));
    }

    #[test]
    fn arena_reports_written_pages_of_all_allocations_in_one_scan_and_reuses_freed_ranges() {
        let Some(a) = arena() else {
            eprintln!("POMINIĘTO: jądro bez userfaultfd WP async / PAGEMAP_SCAN");
            return;
        };
        let (oa, ob) = (a.alloc(PAGE * 3).unwrap(), a.alloc(PAGE * 8).unwrap());
        assert_eq!(ob, oa + PAGE * 3, "przydziały leżą obok siebie");
        let _ = a.take_dirty_all().unwrap(); // stan wyjściowy (inne testy mogą mieć własne przydziały)
        // SAFETY: zapisy w granicach przydziałów.
        unsafe {
            *a.ptr(oa + PAGE + 5) = 1;
            *a.ptr(ob + PAGE * 5) = 2;
        }
        let mine = |v: Vec<(usize, usize)>| -> Vec<(usize, usize)> { v.into_iter().filter(|(x, y)| *x >= oa && *y <= ob + PAGE * 8).collect() };
        assert_eq!(mine(a.take_dirty_all().unwrap()), vec![(oa + PAGE, oa + PAGE * 2), (ob + PAGE * 5, ob + PAGE * 6)]);
        assert!(mine(a.take_dirty_all().unwrap()).is_empty(), "po odczycie stan jest czysty");
        // Zapis pojedynczego przydziału nie jest widoczny w zakresie drugiego.
        unsafe { *a.ptr(oa) = 3 };
        assert_eq!(a.take_dirty_range(ob, PAGE * 8).unwrap(), vec![]);
        assert_eq!(a.take_dirty_range(oa, PAGE * 3).unwrap(), vec![(0, PAGE)]);
        // Zwolnienie i ponowne użycie: zero i czysty stan.
        a.free(oa, PAGE * 3);
        let oc = a.alloc(PAGE * 2).unwrap();
        assert_eq!(oc, oa, "wolny zakres jest używany ponownie");
        assert_eq!(unsafe { *a.ptr(oc) }, 0, "strony po zwolnieniu są wyzerowane");
        assert!(mine(a.take_dirty_all().unwrap()).is_empty());
        unsafe { *a.ptr(oc + PAGE) = 9 };
        assert_eq!(mine(a.take_dirty_all().unwrap()), vec![(oc + PAGE, oc + PAGE * 2)]);
        a.free(oc, PAGE * 2);
        a.free(ob, PAGE * 8);
    }

}

#[cfg(test)]
mod split_tests {
    use super::*;

    #[test]
    fn split_covers_everything_in_order_and_page_aligned() {
        let ext = vec![(0usize, 10 * PAGE), (20 * PAGE, 30 * PAGE), (100 * PAGE, PAGE)];
        for chunk in [PAGE, 3 * PAGE, 7 * PAGE + 1, 64 * PAGE] {
            let flat = split_extents(&ext, chunk);
            let total: usize = flat.iter().map(|e| e.1).sum();
            assert_eq!(total, 41 * PAGE);
            assert!(flat.iter().all(|(o, l)| o % PAGE == 0 && l % PAGE == 0 && *l > 0 && *l <= chunk.div_ceil(PAGE) * PAGE));
            assert!(flat.windows(2).all(|w| w[0].0 + w[0].1 <= w[1].0));
        }
    }

    #[test]
    fn queue_never_deadlocks_when_workers_idle_and_caller_helps() {
        // Odtwarza wzorzec par_scan bez jądra: wątki robocze czekają na Condvar, wołający dokłada zadania i sam je dobiera.
        let q: &'static JobQueue = Box::leak(Box::new(JobQueue { q: Default::default(), cv: Default::default() }));
        for _ in 0..3 {
            std::thread::spawn(move || loop {
                let (i, _, reply) = q.wait_pop();
                let _ = reply.send((i, Some(vec![(i, i)])));
            });
        }
        for round in 0..2000 {
            let (rtx, rrx) = std::sync::mpsc::channel();
            let n = 1 + round % 40;
            q.push_all((0..n).map(|i| (i, (0, 0), rtx.clone())).collect());
            drop(rtx);
            while let Some((i, _, reply)) = q.try_pop() {
                let _ = reply.send((i, Some(vec![(i, i)])));
            }
            let mut got = 0;
            for _ in 0..n {
                rrx.recv_timeout(std::time::Duration::from_secs(5)).expect("zakleszczenie kolejki skanu");
                got += 1;
            }
            assert_eq!(got, n);
        }
    }

    #[test]
    fn split_makes_equal_chunks() {
        let flat = split_extents(&[(0, 400 * PAGE)], 100 * PAGE);
        assert_eq!(flat.len(), 4);
        assert!(flat.iter().all(|c| c.1 == 100 * PAGE));
    }
}

#[cfg(test)]
mod hotcold_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::Arc;

    /// Wyrocznia: pisarze zapisują losowo w arenie, skan odtwarza kopię; po zatrzymaniu pisarzy kopia musi być identyczna z pamięcią.
    /// Pomijany, gdy uffd w trybie jądrowym jest niedostępny (vm.unprivileged_userfaultfd=0).
    fn oracle(cool_after: u64, period: u64, writers: usize, rounds: usize, use_range_scans: bool, wide_pct: u64) {
        if crate::coldtrap::ColdTrap::new().is_none() {
            eprintln!("pominięto: uffd w trybie jądrowym niedostępny");
            return;
        }
        let (c0, h0, f0) = (HC_COOLS.load(Relaxed), HC_HEATS.load(Relaxed), HC_FAULT_PAGES.load(Relaxed));
        let a = Arena::with_len(256 << 20, Some(HcConfig { cool_after, period })).expect("arena");
        let sizes = [24usize << 20, 10 << 20, 3 << 20];
        let offs: Vec<usize> = sizes.iter().map(|&s| a.alloc(s).expect("alloc")).collect();
        let total_end = offs.iter().zip(&sizes).map(|(o, s)| o + s).max().unwrap();
        let base = a.base;
        let mut shadow = vec![0u8; total_end];
        let stop = Arc::new(AtomicBool::new(false));
        let mut hs = Vec::new();
        for w in 0..writers {
            let (stop, offs, sizes, wide_pct) = (stop.clone(), offs.clone(), sizes, wide_pct);
            hs.push(std::thread::spawn(move || {
                let mut x = 0x9E3779B97F4A7C15u64 ^ (w as u64 + 1).wrapping_mul(0xD1B54A32D192ED03);
                let mut n = 0u64;
                while !stop.load(Relaxed) {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let r = (x >> 8) as usize;
                    let ai = (x as usize >> 3) % 3;
                    // `wide_pct` zapisów w całym przydziale (budzą zimne bloki), reszta w małym oknie (gorące).
                    let span = if x % 100 >= wide_pct { (256 * PAGE).min(sizes[ai]) } else { sizes[ai] };
                    let window = ((n / 4000) as usize * 64 * PAGE) % sizes[ai].saturating_sub(span).max(1);
                    let off = offs[ai] + window + r % span;
                    // SAFETY: adres leży w przydziale areny, który żyje do końca testu.
                    unsafe { std::ptr::write_volatile((base + off) as *mut u8, (x >> 56) as u8 | 1) };
                    n += 1;
                    if n % 64 == 0 {
                        std::thread::yield_now();
                    }
                }
            }));
        }
        let sync = |shadow: &mut Vec<u8>, ranges: Vec<(usize, usize)>| {
            for (s, e) in ranges {
                // SAFETY: zakres leży w przydziałach areny.
                let src = unsafe { std::slice::from_raw_parts((base + s) as *const u8, e - s) };
                shadow[s..e].copy_from_slice(src);
            }
        };
        let mut px = 0xA5A5_1234_5678_9ABCu64;
        for i in 0..rounds {
            if wide_pct == 0 {
                // zimne bloki budzimy z wątku głównego: jeden zapis w losowym miejscu co rundę
                px ^= px << 13;
                px ^= px >> 7;
                px ^= px << 17;
                let ai = (px as usize >> 5) % 3;
                let off = offs[ai] + (px >> 16) as usize % sizes[ai];
                // SAFETY: adres leży w przydziale areny.
                unsafe { std::ptr::write_volatile((base + off) as *mut u8, (px >> 56) as u8 | 1) };
            }
            if use_range_scans && i % 3 == 1 {
                for (&o, &s) in offs.iter().zip(&sizes) {
                    let r = a.take_dirty_range(o, s).expect("scan range");
                    sync(&mut shadow, r.into_iter().map(|(x, y)| (x + o, y + o)).collect());
                }
            } else {
                let r = a.take_dirty_all().expect("scan all");
                sync(&mut shadow, r);
            }
            std::thread::sleep(std::time::Duration::from_micros(300));
        }
        stop.store(true, Relaxed);
        for h in hs {
            h.join().unwrap();
        }
        // dwa końcowe skany (drugi łapie ewentualne ogrzania z pierwszego)
        for _ in 0..2 {
            let r = a.take_dirty_all().expect("final scan");
            sync(&mut shadow, r);
        }
        // SAFETY: jak wyżej.
        let actual = unsafe { std::slice::from_raw_parts(base as *const u8, total_end) };
        let bad: Vec<usize> = (0..total_end).filter(|&i| actual[i] != shadow[i]).take(5).collect();
        assert!(bad.is_empty(), "pominięte zapisy pod offsetami {bad:?}");
        eprintln!("oracle cool_after={cool_after} period={period}: ochłodzeń {}, ogrzań {}, stron z pułapki {}", HC_COOLS.load(Relaxed) - c0, HC_HEATS.load(Relaxed) - h0, HC_FAULT_PAGES.load(Relaxed) - f0);
        assert!(!a.hc.as_ref().unwrap().broken.load(Relaxed), "śledzenie zepsute");
    }

    #[test]
    fn no_write_is_lost_with_aggressive_cooling() {
        oracle(2, 1, 3, 600, false, 15);
    }

    #[test]
    fn no_write_is_lost_with_range_scans_and_slow_cooling() {
        oracle(5, 2, 2, 800, true, 0);
    }

    #[test]
    fn free_heats_cold_blocks_and_the_range_can_be_reused() {
        if crate::coldtrap::ColdTrap::new().is_none() {
            return;
        }
        let a = Arena::with_len(64 << 20, Some(HcConfig { cool_after: 1, period: 1 })).expect("arena");
        let o = a.alloc(8 << 20).expect("alloc");
        for _ in 0..6 {
            a.take_dirty_all().expect("scan");
        }
        assert!(HC_COLD_BLOCKS.load(Relaxed) > 0 || HC_COOLS.load(Relaxed) > 0, "bloki powinny ostygnąć");
        a.free(o, 8 << 20);
        let o2 = a.alloc(8 << 20).expect("realloc");
        // SAFETY: świeży przydział areny.
        unsafe { std::ptr::write_volatile(a.ptr(o2 + 12345), 7) };
        let r = a.take_dirty_all().expect("scan");
        assert!(r.iter().any(|&(s, e)| s <= o2 + 12345 && o2 + 12345 < e), "zapis do ponownie użytego zakresu musi być widoczny");
    }
}
