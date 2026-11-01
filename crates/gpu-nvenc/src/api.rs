//! Bezpieczna nakładka na NVENC (`libnvidia-encode.so.1`, przez `dlopen`): lista funkcji, sesja z kontekstem CUDA,
//! zapytania o możliwości. Struktury pochodzą z `sys.rs` (wygenerowanego z nagłówka MIT 12.2).

use std::ffi::c_void;

use crate::consts::*;
use crate::dl::Lib;
use crate::sys::*;

type FnCreateInstance = unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS;
type FnMaxVersion = unsafe extern "C" fn(*mut u32) -> NVENCSTATUS;

pub struct Nvenc {
    fl: NV_ENCODE_API_FUNCTION_LIST,
    max_version: u32,
}

/// Opis błędu NVENC po kodzie (najczęstsze); pozostałe jako liczba.
pub fn status_name(s: NVENCSTATUS) -> String {
    let name = match s {
        0 => "SUCCESS",
        1 => "NO_ENCODE_DEVICE",
        2 => "UNSUPPORTED_DEVICE",
        3 => "INVALID_ENCODERDEVICE",
        4 => "INVALID_DEVICE",
        5 => "DEVICE_NOT_EXIST",
        6 => "INVALID_PTR",
        7 => "INVALID_EVENT",
        8 => "INVALID_PARAM",
        9 => "INVALID_CALL",
        10 => "OUT_OF_MEMORY",
        11 => "ENCODER_NOT_INITIALIZED",
        12 => "UNSUPPORTED_PARAM",
        13 => "LOCK_BUSY",
        14 => "NOT_ENOUGH_BUFFER",
        15 => "INVALID_VERSION",
        16 => "MAP_FAILED",
        17 => "NEED_MORE_INPUT",
        18 => "ENCODER_BUSY",
        _ => return format!("NVENCSTATUS {s}"),
    };
    format!("NV_ENC_ERR_{name}")
}

pub(crate) fn ck(what: &str, s: NVENCSTATUS) -> Result<(), String> {
    if s == 0 {
        Ok(())
    } else {
        Err(format!("{what}: {}", status_name(s)))
    }
}

/// Wersja API jako `major.minor` z wartości `NvEncodeAPIGetMaxSupportedVersion` (`(major << 4) | minor`).
pub fn version_string(v: u32) -> String {
    format!("{}.{}", v >> 4, v & 0xF)
}

impl Nvenc {
    pub fn load() -> Result<Nvenc, String> {
        let lib = Lib::open(&["libnvidia-encode.so.1", "libnvidia-encode.so"])?;
        let create = lib.sym("NvEncodeAPICreateInstance").ok_or("libnvidia-encode: brak NvEncodeAPICreateInstance")?;
        let maxv = lib.sym("NvEncodeAPIGetMaxSupportedVersion").ok_or("libnvidia-encode: brak NvEncodeAPIGetMaxSupportedVersion")?;
        // SAFETY: sygnatury z dokumentacji NVENC.
        let (create, maxv) = unsafe { (std::mem::transmute::<*mut c_void, FnCreateInstance>(create), std::mem::transmute::<*mut c_void, FnMaxVersion>(maxv)) };
        let mut max_version = 0u32;
        // SAFETY: poprawny wskaźnik wyjściowy.
        ck("NvEncodeAPIGetMaxSupportedVersion", unsafe { maxv(&mut max_version) })?;
        let mut fl = NV_ENCODE_API_FUNCTION_LIST { version: NV_ENCODE_API_FUNCTION_LIST_VER, ..Default::default() };
        // SAFETY: `fl` ma ustawioną wersję, jak wymaga dokumentacja.
        ck("NvEncodeAPICreateInstance", unsafe { create(&mut fl) })?;
        Ok(Nvenc { fl, max_version })
    }

    /// Najwyższa wersja API obsługiwana przez sterownik, `(major << 4) | minor`.
    pub fn max_api_version(&self) -> u32 {
        self.max_version
    }

    /// Otwiera sesję kodowania na kontekście CUDA `cuda_ctx`.
    pub fn open_session(&self, cuda_ctx: *mut c_void) -> Result<Session, String> {
        let open = self.fl.nvEncOpenEncodeSessionEx.ok_or("brak nvEncOpenEncodeSessionEx")?;
        let mut p = _NV_ENC_OPEN_ENCODE_SESSIONEX_PARAMS {
            version: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            deviceType: NV_ENC_DEVICE_TYPE_CUDA,
            device: cuda_ctx,
            apiVersion: NVENCAPI_VERSION,
            ..Default::default()
        };
        let mut enc: *mut c_void = std::ptr::null_mut();
        // SAFETY: struktura ma ustawioną wersję; kontekst CUDA jest ważny.
        ck("nvEncOpenEncodeSessionEx", unsafe { open(&mut p, &mut enc) })?;
        Ok(Session { fl: self.fl, enc })
    }
}

pub struct Session {
    /// Kopia tablicy funkcji (wskaźniki są stałe do końca życia procesu), więc sesja nie pożycza `Nvenc`.
    pub(crate) fl: NV_ENCODE_API_FUNCTION_LIST,
    pub(crate) enc: *mut c_void,
}

// SAFETY: sesja NVENC wolno używać z jednego wątku naraz (wymaga `&mut`/wyłączności po stronie wołającego).
unsafe impl Send for Session {}

impl Session {
    /// Lista GUID-ów obsługiwanych kodeków.
    pub fn codec_guids(&self) -> Result<Vec<GUID>, String> {
        let count = self.fl.nvEncGetEncodeGUIDCount.ok_or("brak nvEncGetEncodeGUIDCount")?;
        let get = self.fl.nvEncGetEncodeGUIDs.ok_or("brak nvEncGetEncodeGUIDs")?;
        let mut n = 0u32;
        // SAFETY: sesja jest ważna, wskaźniki wyjściowe poprawne.
        ck("nvEncGetEncodeGUIDCount", unsafe { count(self.enc, &mut n) })?;
        let mut v = vec![GUID::default(); n as usize];
        let mut got = 0u32;
        // SAFETY: bufor ma miejsce na `n` GUID-ów.
        ck("nvEncGetEncodeGUIDs", unsafe { get(self.enc, v.as_mut_ptr(), n, &mut got) })?;
        v.truncate(got as usize);
        Ok(v)
    }

    /// Wartość możliwości `cap` (np. `NV_ENC_CAPS_WIDTH_MAX`) dla kodeka `codec`.
    pub fn cap(&self, codec: GUID, cap: NV_ENC_CAPS) -> Result<i32, String> {
        let f = self.fl.nvEncGetEncodeCaps.ok_or("brak nvEncGetEncodeCaps")?;
        let mut p = NV_ENC_CAPS_PARAM { version: NV_ENC_CAPS_PARAM_VER, capsToQuery: cap, ..Default::default() };
        let mut val = 0i32;
        // SAFETY: struktura ma wersję; wskaźnik wyjściowy poprawny.
        ck("nvEncGetEncodeCaps", unsafe { f(self.enc, codec, &mut p, &mut val) })?;
        Ok(val)
    }

    /// Formaty wejściowe obsługiwane dla kodeka (wartości `NV_ENC_BUFFER_FORMAT_*`).
    pub fn input_formats(&self, codec: GUID) -> Result<Vec<u32>, String> {
        let count = self.fl.nvEncGetInputFormatCount.ok_or("brak nvEncGetInputFormatCount")?;
        let get = self.fl.nvEncGetInputFormats.ok_or("brak nvEncGetInputFormats")?;
        let mut n = 0u32;
        // SAFETY: jak wyżej.
        ck("nvEncGetInputFormatCount", unsafe { count(self.enc, codec, &mut n) })?;
        let mut v = vec![0 as NV_ENC_BUFFER_FORMAT; n as usize];
        let mut got = 0u32;
        // SAFETY: bufor ma miejsce na `n` wartości.
        ck("nvEncGetInputFormats", unsafe { get(self.enc, codec, v.as_mut_ptr(), n, &mut got) })?;
        v.truncate(got as usize);
        Ok(v)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(d) = self.fl.nvEncDestroyEncoder {
            // SAFETY: sesja pochodzi z `open_session` i jest niszczona raz.
            unsafe { d(self.enc) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_status_formatting() {
        assert_eq!(version_string(0xD0), "13.0");
        assert_eq!(version_string(0xC2), "12.2");
        assert_eq!(status_name(0), "NV_ENC_ERR_SUCCESS");
        assert_eq!(status_name(15), "NV_ENC_ERR_INVALID_VERSION");
        assert_eq!(status_name(999), "NVENCSTATUS 999");
    }

    #[test]
    fn generated_constants_match_the_header_layout_rules() {
        // NVENCAPI_VERSION = major | (minor << 24) dla 12.2.
        assert_eq!(NVENCAPI_VERSION & 0xFF, 12);
        assert_eq!(NVENCAPI_VERSION >> 24, 2);
        // Wersje struktur niosą wersję API w najniższych bitach i znacznik 0x7 w bitach 28-30.
        for v in [NV_ENCODE_API_FUNCTION_LIST_VER, NV_ENC_CONFIG_VER, NV_ENC_INITIALIZE_PARAMS_VER, NV_ENC_PIC_PARAMS_VER] {
            assert_eq!(v & 0xFFFF, NVENCAPI_VERSION & 0xFFFF);
            assert_eq!((v >> 28) & 0x7, 0x7);
        }
        assert_eq!(NV_ENC_CODEC_H264_GUID.Data1, 0x6bc8_2762);
    }

    #[test]
    fn real_driver_probe_when_nvenc_is_present() {
        let Ok(nv) = Nvenc::load() else {
            eprintln!("POMINIĘTO: brak libnvidia-encode");
            return;
        };
        assert!(nv.max_api_version() >= 0xC0, "sterownik obsługuje co najmniej NVENC API 12.0");
    }
}
