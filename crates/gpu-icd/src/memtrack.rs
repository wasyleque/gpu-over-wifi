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

/// Bufor wyrównany do strony, wyzerowany (świeże strony od jądra), o rozmiarze zaokrąglonym w górę do strony.
pub struct PageAlignedBuf {
    ptr: *mut u8,
    len: usize,
    cap: usize,
    /// Podwójne odwzorowanie `memfd` (odczyt leniwy): `ptr` to widok własny, widok aplikacji zwraca `app_ptr`.
    shared: Option<crate::shadow::DoubleMap>,
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
        PageAlignedBuf { ptr, len, cap, shared: None }
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
        Some(PageAlignedBuf { ptr, len, cap, shared: Some(dm) })
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

}
