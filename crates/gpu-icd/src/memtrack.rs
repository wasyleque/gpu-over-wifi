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
            let n = unsafe { ioctl(self.pagemap.as_raw_fd(), PAGEMAP_SCAN, &mut arg as *mut PmScanArg) };
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

/// Jedna wspólna arena adresów dla kopii wszystkich odwzorowanych pamięci: jeden zarejestrowany obszar, więc ślad zapisów całej gry pobieramy jednym
/// (lub kilkoma przy dziurach) wywołaniem `PAGEMAP_SCAN` zamiast wywołania na każdą pamięć (w Cyberpunku ponad 200 skanów na klatkę po ok. 60 µs,
/// w tym haszowanie małych pamięci bez śledzenia jądrowego).
pub struct Arena {
    base: usize,
    tracker: Tracker,
    st: std::sync::Mutex<ArenaState>,
}

struct ArenaState {
    /// Wolne zakresy (offset -> długość) i zajęte (offset -> długość).
    free: std::collections::BTreeMap<usize, usize>,
    used: std::collections::BTreeMap<usize, usize>,
}

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
        for len in [64usize << 30, 16 << 30, 4 << 30] {
            // PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE: adresy bez rezerwacji pamięci, strony powstają przy pierwszym zapisie.
            // SAFETY: anonimowe odwzorowanie bez adresu docelowego.
            let ptr = unsafe { mmap(std::ptr::null_mut(), len, 3, 0x22 | 0x4000, -1, 0) };
            if ptr as isize == -1 {
                continue;
            }
            let Some(tracker) = Tracker::new_unarmed(ptr, len) else {
                // SAFETY: zwalniamy odwzorowanie utworzone powyżej.
                unsafe { munmap(ptr, len) };
                return None;
            };
            let mut free = std::collections::BTreeMap::new();
            free.insert(0, len);
            return Some(Arena { base: ptr as usize, tracker, st: std::sync::Mutex::new(ArenaState { free, used: Default::default() }) });
        }
        None
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
        Some(off)
    }

    /// Zwalnia zakres (strony wracają do systemu) i skleja sąsiednie wolne.
    pub fn free(&self, off: usize, len: usize) {
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
        for (o, l) in extents {
            out.extend(self.tracker.take_dirty(o, l)?);
        }
        Some(out)
    }

    /// Jak `take_dirty_all`, ale tylko dla jednego przydziału (`Unmap` jednej pamięci); zakresy względem początku przydziału.
    pub fn take_dirty_range(&self, off: usize, len: usize) -> Option<Vec<(usize, usize)>> {
        self.tracker.take_dirty(off, len).map(|v| v.into_iter().map(|(a, b)| (a - off, b - off)).collect())
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
