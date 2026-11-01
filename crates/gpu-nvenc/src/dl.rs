//! Ładowanie bibliotek współdzielonych przez `dlopen` (bez linkowania w czasie budowania: serwer bez NVIDII nadal się zbuduje).

use std::ffi::{c_char, c_int, c_void, CStr, CString};

const RTLD_NOW: c_int = 2;
const RTLD_LOCAL: c_int = 0;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

/// Uchwyt biblioteki; celowo nigdy nie zamykany (rozładowanie sterownika w trakcie działania procesu jest niebezpieczne).
#[derive(Clone, Copy)]
pub struct Lib {
    handle: *mut c_void,
}

// SAFETY: uchwyt biblioteki jest tylko do odczytu (dlsym jest wątkowo-bezpieczne).
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

impl Lib {
    /// Otwiera pierwszą działającą bibliotekę z listy nazw.
    pub fn open(names: &[&str]) -> Result<Lib, String> {
        let mut last = "brak kandydatów".to_string();
        for n in names {
            let Ok(c) = CString::new(*n) else { continue };
            // SAFETY: `c` jest poprawnym napisem C.
            let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
            if !h.is_null() {
                return Ok(Lib { handle: h });
            }
            // SAFETY: `dlerror` zwraca null albo poprawny napis C.
            last = unsafe {
                let e = dlerror();
                if e.is_null() { format!("{n}: nieznany błąd") } else { CStr::from_ptr(e).to_string_lossy().into_owned() }
            };
        }
        Err(last)
    }

    /// Adres symbolu albo `None`.
    pub fn sym(&self, name: &str) -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        // SAFETY: uchwyt pochodzi z udanego `dlopen`, `c` jest poprawnym napisem C.
        let p = unsafe { dlsym(self.handle, c.as_ptr()) };
        (!p.is_null()).then_some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_libc_and_finds_symbols_and_reports_missing_ones() {
        let l = Lib::open(&["libnie-istnieje.so.9", "libc.so.6"]).expect("libc");
        assert!(l.sym("strlen").is_some());
        assert!(l.sym("nie_ma_takiego_symbolu_xyz").is_none());
        assert!(Lib::open(&["libnie-istnieje.so.9"]).is_err());
        assert!(Lib::open(&[]).is_err());
        assert!(l.sym("a\0b").is_none());
    }
}
