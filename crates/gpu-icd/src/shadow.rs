//! Podwójne odwzorowanie tej samej pamięci anonimowej (`memfd_create` + dwa razy `mmap(MAP_SHARED)`):
//! widok aplikacji (`app_ptr`, zwracany z `vkMapMemory`, objęty zrzucaniem stron i obsługą błędów stron)
//! oraz widok własny (`own_ptr`, przez który sterownik zawsze czyta i pisze bez wyzwalania błędów stron).

#![allow(dead_code, clippy::len_without_is_empty)]

use std::io::{Error, ErrorKind, Result};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub const PAGE: usize = 4096;

const SYS_MEMFD_CREATE: i64 = 319;
const MFD_CLOEXEC: u32 = 1;
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_SHARED: i32 = 1;
const MADV_DONTNEED: i32 = 4;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn ftruncate(fd: i32, length: i64) -> i32;
    fn syscall(num: i64, ...) -> i64;
}

/// Dwa współdzielone odwzorowania (`MAP_SHARED`) jednego pliku anonimowego `memfd`.
pub struct DoubleMap {
    _fd: OwnedFd,
    app: *mut u8,
    own: *mut u8,
    len: usize,
}

// SAFETY: wskaźniki `app` i `own` odnoszą się do odwzorowań jądra żyjących dokładnie tak długo jak `DoubleMap`;
// synchronizację dostępu do zawartości pamięci zapewnia właściciel struktury.
unsafe impl Send for DoubleMap {}
// SAFETY: metody `&self` zwracają jedynie metadane lub wykonują atomowe wywołanie jądra `madvise` na ustalonym zakresie;
// synchronizację dostępu do danych pod wskaźnikami zapewnia użytkownik.
unsafe impl Sync for DoubleMap {}

impl DoubleMap {
    /// Tworzy podwójne odwzorowanie o rozmiarze `len` zaokrąglonym w górę do 4096 bajtów (`len == 0` -> `Err(InvalidInput)`).
    pub fn new(len: usize) -> Result<DoubleMap> {
        if len == 0 {
            return Err(Error::from(ErrorKind::InvalidInput));
        }
        let Some(aligned) = len.checked_next_multiple_of(PAGE) else {
            return Err(Error::from(ErrorKind::InvalidInput));
        };
        if aligned > i64::MAX as usize {
            return Err(Error::from(ErrorKind::InvalidInput));
        }

        // SAFETY: `syscall(SYS_memfd_create)` z poprawnym wskaźnikiem C-stringa i flagą `MFD_CLOEXEC`;
        // zwrócony deskryptor natychmiast przejmuje `OwnedFd`.
        let fd = unsafe {
            let raw = syscall(SYS_MEMFD_CREATE, c"gpu-icd-shadow".as_ptr(), MFD_CLOEXEC);
            if raw < 0 {
                return Err(Error::last_os_error());
            }
            OwnedFd::from_raw_fd(raw as i32)
        };

        // SAFETY: `fd` to świeżo utworzony deskryptor memfd, a `aligned` mieści się w `i64`.
        if unsafe { ftruncate(fd.as_raw_fd(), aligned as i64) } < 0 {
            return Err(Error::last_os_error());
        }

        let prot = PROT_READ | PROT_WRITE;
        // SAFETY: odwzorowanie dzielone prawidłowego pliku `fd` o rozmiarze `aligned` bez narzuconego adresu.
        let app = unsafe { mmap(std::ptr::null_mut(), aligned, prot, MAP_SHARED, fd.as_raw_fd(), 0) };
        if app as isize == -1 {
            return Err(Error::last_os_error());
        }

        // SAFETY: drugie niezależne odwzorowanie tego samego deskryptora `fd`; w razie błędu zwalniamy pierwsze.
        let own = unsafe { mmap(std::ptr::null_mut(), aligned, prot, MAP_SHARED, fd.as_raw_fd(), 0) };
        if own as isize == -1 {
            let err = Error::last_os_error();
            // SAFETY: `app` pochodzi z udanego `mmap` o długości `aligned`.
            unsafe { munmap(app, aligned) };
            return Err(err);
        }

        Ok(DoubleMap { _fd: fd, app, own, len: aligned })
    }

    /// Rozmiar odwzorowania w bajtach (po zaokrągleniu w górę do rozmiaru strony).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Wskaźnik na widok aplikacji (przekazywany z `vkMapMemory`).
    pub fn app_ptr(&self) -> *mut u8 {
        self.app
    }

    /// Wskaźnik na widok własny sterownika.
    pub fn own_ptr(&self) -> *mut u8 {
        self.own
    }

    /// Zrzuca tablice stron widoku aplikacji dla `[offset, offset + len)` (`madvise(MADV_DONTNEED)`):
    /// dane w pliku `memfd` zostają, ale kolejny dostęp przez `app_ptr` wywoła ponowne mapowanie (błąd strony).
    /// `offset` i `len` muszą być wielokrotnościami 4096 i mieścić się w obszarze, inaczej `Err(InvalidInput)`.
    pub fn zap_app_view(&self, offset: usize, len: usize) -> Result<()> {
        if !offset.is_multiple_of(PAGE) || !len.is_multiple_of(PAGE) {
            return Err(Error::from(ErrorKind::InvalidInput));
        }
        let Some(end) = offset.checked_add(len) else {
            return Err(Error::from(ErrorKind::InvalidInput));
        };
        if end > self.len {
            return Err(Error::from(ErrorKind::InvalidInput));
        }
        if len == 0 {
            return Ok(());
        }
        // SAFETY: `offset` jest wyrównany do strony, a `[offset, offset + len)` mieści się w odwzorowaniu `app` o rozmiarze `self.len`.
        let rc = unsafe { madvise(self.app.add(offset), len, MADV_DONTNEED) };
        if rc < 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for DoubleMap {
    fn drop(&mut self) {
        // SAFETY: oba wskaźniki pochodzą z `mmap` o długości `self.len` w `DoubleMap::new`.
        unsafe {
            munmap(self.app, self.len);
            munmap(self.own, self.len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_to_own_view_are_visible_in_app_view_and_vice_versa() {
        let dm = DoubleMap::new(PAGE * 2).unwrap();
        assert_ne!(dm.app_ptr(), dm.own_ptr(), "oba widoki mają różne adresy wirtualne");

        // SAFETY: odczyt i zapis w granicach `dm.len()`.
        unsafe {
            let own = std::slice::from_raw_parts_mut(dm.own_ptr(), dm.len());
            let app = std::slice::from_raw_parts_mut(dm.app_ptr(), dm.len());
            assert!(own.iter().all(|&b| b == 0) && app.iter().all(|&b| b == 0), "nowy memfd jest wyzerowany");

            own[0] = 0xAA;
            own[PAGE + 17] = 0xBB;
            assert_eq!(app[0], 0xAA);
            assert_eq!(app[PAGE + 17], 0xBB);

            app[123] = 0xCC;
            app[PAGE * 2 - 1] = 0xDD;
            assert_eq!(own[123], 0xCC);
            assert_eq!(own[PAGE * 2 - 1], 0xDD);
        }
    }

    #[test]
    fn len_is_rounded_up_to_page_and_zero_len_is_rejected() {
        assert_eq!(DoubleMap::new(0).err().map(|e| e.kind()), Some(ErrorKind::InvalidInput));
        let dm1 = DoubleMap::new(1).unwrap();
        assert_eq!(dm1.len(), PAGE);
        let dm2 = DoubleMap::new(PAGE + 5).unwrap();
        assert_eq!(dm2.len(), PAGE * 2);
        let dm3 = DoubleMap::new(PAGE * 3).unwrap();
        assert_eq!(dm3.len(), PAGE * 3);
    }

    #[test]
    fn zap_app_view_preserves_backing_data_in_both_views() {
        let dm = DoubleMap::new(PAGE * 3).unwrap();
        // SAFETY: zakres mieści się w `dm.len()`.
        unsafe {
            let own = std::slice::from_raw_parts_mut(dm.own_ptr(), dm.len());
            let app = std::slice::from_raw_parts(dm.app_ptr(), dm.len());
            for (i, b) in own.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
            // Dotknij stron widoku aplikacji, żeby zmaterializować tablice stron przed zrzuceniem.
            assert_eq!(app[PAGE + 42], ((PAGE + 42) % 251) as u8);

            dm.zap_app_view(PAGE, PAGE).unwrap();

            // Po MADV_DONTNEED na MAP_SHARED dane w pliku memfd zostają, a ponowny odczyt z `app` przywraca stronę z pliku.
            assert_eq!(own[PAGE + 42], ((PAGE + 42) % 251) as u8);
            assert_eq!(app[PAGE + 42], ((PAGE + 42) % 251) as u8);
            assert_eq!(app[0], 0);
            assert_eq!(app[PAGE * 2 + 7], ((PAGE * 2 + 7) % 251) as u8);
        }
    }

    #[test]
    fn zap_app_view_rejects_misaligned_or_out_of_bounds_ranges() {
        let dm = DoubleMap::new(PAGE * 2).unwrap();
        assert_eq!(dm.zap_app_view(1, PAGE).unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(dm.zap_app_view(0, PAGE - 1).unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(dm.zap_app_view(PAGE, PAGE * 2).unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(dm.zap_app_view(PAGE * 3, PAGE).unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(dm.zap_app_view(usize::MAX - PAGE + 1, PAGE * 2).unwrap_err().kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn repeated_create_and_drop_does_not_leak_descriptors() {
        for i in 0..200 {
            let dm = DoubleMap::new(PAGE * 2).unwrap();
            // SAFETY: zapis i odczyt pierwszego bajtu w granicach odwzorowania.
            unsafe {
                *dm.own_ptr() = i as u8;
                assert_eq!(*dm.app_ptr(), i as u8);
            }
        }
    }
}
