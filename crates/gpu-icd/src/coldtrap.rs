//! Pułapka synchroniczna dla zimnych bloków areny: drugi `userfaultfd` w trybie jądrowym (bez `UFFD_USER_MODE_ONLY`) i bez `WP_ASYNC`.
//! Zapis do chronionej strony (także z jądra, np. `read()` do bufora aplikacji) zatrzymuje piszącego do czasu, aż wątek obsługi zapisze adres
//! strony na liście oczekujących i zdejmie z niej ochronę. Skan czyta tę listę przed odczytem zawartości, więc żaden zapis nie umyka.
//! Wymaga `vm.unprivileged_userfaultfd=1` albo uprawnień (`CAP_SYS_PTRACE`); bez nich `ColdTrap::new` zwraca `None`.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

const SYS_USERFAULTFD: i64 = 323;
const O_CLOEXEC: i64 = 0o2000000;
const O_NONBLOCK: i64 = 0o4000;
const UFFD_API: u64 = 0xAA;
const UFFD_FEATURE_WP_UNPOPULATED: u64 = 1 << 13;
const UFFDIO_API: u64 = 0xc018_aa3f;
const UFFDIO_REGISTER: u64 = 0xc020_aa00;
const UFFDIO_UNREGISTER: u64 = 0x8010_aa01;
const UFFDIO_WAKE: u64 = 0x8010_aa02;
const UFFDIO_WRITEPROTECT: u64 = 0xc018_aa06;
const UFFDIO_REGISTER_MODE_WP: u64 = 2;
const UFFDIO_WRITEPROTECT_MODE_WP: u64 = 1;
const UFFD_EVENT_PAGEFAULT: u8 = 0x12;
const PAGE: usize = 4096;

extern "C" {
    fn syscall(num: i64, ...) -> i64;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn read(fd: i32, buf: *mut std::ffi::c_void, n: usize) -> isize;
    fn poll(fds: *mut PollFd, n: u64, timeout: i32) -> i32;
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[repr(C)]
struct Api {
    api: u64,
    features: u64,
    ioctls: u64,
}

#[repr(C)]
struct Register {
    start: u64,
    len: u64,
    mode: u64,
    ioctls: u64,
}

#[repr(C)]
struct Range {
    start: u64,
    len: u64,
}

#[repr(C)]
struct WriteProtect {
    start: u64,
    len: u64,
    mode: u64,
}

pub struct ColdTrap {
    fd: Arc<OwnedFd>,
    /// Adresy stron (bezwzględne), do których aplikacja zapisała w zimnych blokach od ostatniego odczytu listy.
    pending: Arc<Mutex<Vec<usize>>>,
}

fn unprotect(fd: i32, page: usize) -> bool {
    let mut wp = WriteProtect { start: page as u64, len: PAGE as u64, mode: 0 };
    // SAFETY: struktura żyje przez czas wywołania; zakres to jedna strona.
    let ok = unsafe { ioctl(fd, UFFDIO_WRITEPROTECT, &mut wp as *mut WriteProtect) >= 0 };
    if !ok {
        // Strona nie jest już zarejestrowana (blok zdążył zostać ogrzany): jądro mogło zaparkować piszącego, który ponowił błąd w trakcie
        // wyrejestrowywania, więc budzimy go jawnie; inaczej zostałby w kolejce na zawsze.
        let mut r = Range { start: page as u64, len: PAGE as u64 };
        // SAFETY: `r` to `struct uffdio_range`.
        unsafe { ioctl(fd, UFFDIO_WAKE, &mut r as *mut Range) };
    }
    ok
}

impl ColdTrap {
    pub fn new() -> Option<ColdTrap> {
        // SAFETY: wywołania systemowe na poprawnych strukturach; deskryptor jest zamykany przez `OwnedFd`.
        let fd = unsafe {
            let fd = syscall(SYS_USERFAULTFD, O_CLOEXEC | O_NONBLOCK);
            if fd < 0 {
                return None;
            }
            let fd = OwnedFd::from_raw_fd(fd as i32);
            let mut api = Api { api: UFFD_API, features: UFFD_FEATURE_WP_UNPOPULATED, ioctls: 0 };
            if ioctl(fd.as_raw_fd(), UFFDIO_API, &mut api as *mut Api) < 0 {
                return None;
            }
            fd
        };
        let fd = Arc::new(fd);
        let pending = Arc::new(Mutex::new(Vec::new()));
        let (tfd, tpend) = (fd.clone(), pending.clone());
        std::thread::Builder::new().name("icd-coldtrap".into()).spawn(move || Self::handler(tfd, tpend)).ok()?;
        Some(ColdTrap { fd, pending })
    }

    fn handler(fd: Arc<OwnedFd>, pending: Arc<Mutex<Vec<usize>>>) {
        let raw = fd.as_raw_fd();
        let mut msg = [0u8; 32];
        loop {
            let mut p = PollFd { fd: raw, events: 1, revents: 0 };
            // SAFETY: `p` żyje przez czas wywołania.
            if unsafe { poll(&mut p, 1, 1000) } < 0 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            loop {
                // SAFETY: bufor ma 32 bajty = rozmiar `struct uffd_msg`.
                let n = unsafe { read(raw, msg.as_mut_ptr().cast(), msg.len()) };
                if n != 32 {
                    break;
                }
                if msg[0] != UFFD_EVENT_PAGEFAULT {
                    continue;
                }
                let addr = u64::from_ne_bytes(msg[16..24].try_into().unwrap_or([0; 8])) as usize & !(PAGE - 1);
                // Najpierw lista, potem zdjęcie ochrony: zapis może wylądować dopiero po `unprotect`, a skan czyta listę przed odczytem danych.
                pending.lock().unwrap_or_else(|e| e.into_inner()).push(addr);
                // Gdy strona nie jest już zarejestrowana w tym uffd (blok zdążył zostać ogrzany), `unprotect` się nie powiedzie, ale ogrzanie obudziło piszącego.
                let _ = unprotect(raw, addr);
            }
        }
    }

    /// Rejestruje `[start, start+len)` i włącza ochronę zapisu. `false`, gdy się nie powiodło (zakres zostaje niezarejestrowany).
    pub fn protect(&self, start: usize, len: usize) -> bool {
        let fd = self.fd.as_raw_fd();
        let mut reg = Register { start: start as u64, len: len as u64, mode: UFFDIO_REGISTER_MODE_WP, ioctls: 0 };
        // SAFETY: struktury żyją przez czas wywołań.
        unsafe {
            if ioctl(fd, UFFDIO_REGISTER, &mut reg as *mut Register) < 0 {
                return false;
            }
            let mut wp = WriteProtect { start: start as u64, len: len as u64, mode: UFFDIO_WRITEPROTECT_MODE_WP };
            if ioctl(fd, UFFDIO_WRITEPROTECT, &mut wp as *mut WriteProtect) < 0 {
                let mut r = Range { start: start as u64, len: len as u64 };
                ioctl(fd, UFFDIO_UNREGISTER, &mut r as *mut Range);
                return false;
            }
        }
        true
    }

    #[cfg(test)]
    pub fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    /// Wyrejestrowuje zakres (budzi zatrzymanych piszących; jądro zdejmuje ochronę).
    pub fn release(&self, start: usize, len: usize) -> bool {
        let mut r = Range { start: start as u64, len: len as u64 };
        // SAFETY: struktura żyje przez czas wywołania.
        unsafe { ioctl(self.fd.as_raw_fd(), UFFDIO_UNREGISTER, &mut r as *mut Range) >= 0 }
    }

    /// Wyjmuje z listy strony z `[start, end)`; pozostałe zostają.
    pub fn take_pending_in(&self, start: usize, end: usize) -> Vec<usize> {
        let mut g = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_empty() {
            return Vec::new();
        }
        let (mine, rest): (Vec<usize>, Vec<usize>) = g.drain(..).partition(|a| *a >= start && *a < end);
        *g = rest;
        mine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    }

    fn region(len: usize) -> usize {
        // SAFETY: anonimowe odwzorowanie bez adresu docelowego.
        let p = unsafe { mmap(std::ptr::null_mut(), len, 3, 0x22 | 0x4000, -1, 0) };
        assert_ne!(p as isize, -1);
        p as usize
    }

    #[test]
    fn a_write_to_a_protected_page_is_listed_and_released() {
        let Some(t) = ColdTrap::new() else { return };
        let len = 4 << 20;
        let base = region(len);
        assert!(t.protect(base, len));
        let w = std::thread::spawn(move || {
            for i in 0..8usize {
                // SAFETY: adres w odwzorowaniu zachowanym do końca testu.
                unsafe { std::ptr::write_volatile((base + i * 3 * PAGE + 17) as *mut u8, 1) };
            }
        });
        let t0 = std::time::Instant::now();
        while !w.is_finished() {
            assert!(t0.elapsed().as_secs() < 10, "pisarz nie został obudzony");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        w.join().unwrap();
        let got = t.take_pending_in(base, base + len);
        assert_eq!(got.len(), 8);
    }

    #[test]
    fn release_and_protect_cycles_never_strand_a_writer() {
        let Some(t) = ColdTrap::new() else { return };
        let len = 8usize << 20;
        let base = region(len);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut hs = Vec::new();
        for w in 0..2usize {
            let stop = stop.clone();
            hs.push(std::thread::spawn(move || {
                let mut x = 88172645463325252u64 ^ w as u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    // SAFETY: adres w odwzorowaniu zachowanym do końca testu.
                    unsafe { std::ptr::write_volatile((base + (x as usize % len)) as *mut u8, 1) };
                }
            }));
        }
        let t0 = std::time::Instant::now();
        let mut i = 0usize;
        while t0.elapsed().as_millis() < 1500 {
            let blk = (i * 7) % 8;
            assert!(t.protect(base + blk * (1 << 20), 1 << 20));
            std::thread::sleep(std::time::Duration::from_micros(200));
            assert!(t.release(base + blk * (1 << 20), 1 << 20));
            i += 1;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let t1 = std::time::Instant::now();
        for h in hs {
            while !h.is_finished() {
                assert!(t1.elapsed().as_secs() < 10, "pisarz utknął po cyklach protect/release");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            h.join().unwrap();
        }
    }
}
