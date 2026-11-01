//! Komunikaty wywołań M1 (zapytania o instancję i kartę): żądania klienta i odpowiedzi serwera.
//!
//! Kodowanie: pierwszy bajt = kod operacji (u8), potem pola przez `gpu_wire::Encoder`
//! (liczby LE, napisy i bajty z prefiksem długości). Dane z sieci są niezaufane:
//! dekoder sprawdza limity i odrzuca bajty nadmiarowe po poprawnym komunikacie.

use gpu_wire::WireError;

pub const PROTOCOL_VERSION: u32 = 1;
/// Maksymalna długość tokenu uwierzytelniającego (bajty).
pub const MAX_TOKEN_LEN: usize = 256;
/// Maksymalna liczba nazw rozszerzeń w żądaniu i maksymalna długość jednej nazwy.
pub const MAX_EXTENSIONS: usize = 256;
pub const MAX_NAME_LEN: usize = 256;
/// Maksymalna długość łańcucha `pNext` (liczba struktur `sType`) w jednym zapytaniu.
pub const MAX_CHAIN: usize = 64;
/// Maksymalna liczba elementów w zapytaniu o tablicę (np. rodziny kolejek).
pub const MAX_COUNT: u32 = 1024;
/// Maksymalny rozmiar danych w odpowiedzi (bajty).
pub const MAX_DATA: usize = 4 * 1024 * 1024;

/// Którą funkcję `vkGetPhysicalDevice*` wywołać na serwerze.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PdFunc {
    Properties = 1,
    Properties2 = 2,
    Features = 3,
    Features2 = 4,
    MemoryProperties = 5,
    MemoryProperties2 = 6,
    FormatProperties = 7,
    FormatProperties2 = 8,
    QueueFamilyProperties = 9,
    QueueFamilyProperties2 = 10,
}

impl PdFunc {
    pub fn from_u8(v: u8) -> Option<PdFunc> {
        todo!()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello { version: u32, token: Vec<u8> },
    EnumInstanceVersion,
    EnumInstanceExtensions,
    CreateInstance { api_version: u32, extensions: Vec<String> },
    DestroyInstance { instance: u64 },
    EnumPhysicalDevices { instance: u64 },
    /// `arg`: format dla FormatProperties*; `count`: liczba elementów dla QueueFamilyProperties*;
    /// `chain`: wartości `sType` kolejnych struktur łańcucha `pNext` (dla wariantów `*2`).
    GetPhysicalDevice { pdev: u64, func: PdFunc, arg: u32, count: u32, chain: Vec<u32> },
    EnumDeviceExtensions { pdev: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    HelloOk { version: u32 },
    /// Uchwyt (np. instancja).
    Handle(u64),
    Handles(Vec<u64>),
    U32(u32),
    /// Surowe bajty wyniku (struktury właściwości itp.).
    Data(Vec<u8>),
    /// Błąd: kod `VkResult` (ujemny) lub 0 dla błędu protokołu, plus opis.
    Error { code: i32, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsgError {
    /// Błąd niskiego poziomu kodowania (ucięte dane, zły UTF-8 itd.).
    Wire(WireError),
    UnknownOp(u8),
    UnknownFunc(u8),
    /// Przekroczono limit (token, liczba rozszerzeń, długość nazwy, łańcuch, liczba elementów, dane).
    LimitExceeded(&'static str),
    /// Po poprawnym komunikacie zostały dodatkowe bajty.
    TrailingBytes(usize),
}

impl From<WireError> for MsgError {
    fn from(e: WireError) -> Self {
        MsgError::Wire(e)
    }
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }

    pub fn decode(bytes: &[u8]) -> Result<Request, MsgError> {
        todo!()
    }
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }

    pub fn decode(bytes: &[u8]) -> Result<Response, MsgError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Hello { version: PROTOCOL_VERSION, token: b"sekret".to_vec() },
            Request::EnumInstanceVersion,
            Request::EnumInstanceExtensions,
            Request::CreateInstance { api_version: 0x0040_3000, extensions: vec!["VK_KHR_surface".into(), "VK_EXT_debug_utils".into()] },
            Request::CreateInstance { api_version: 1, extensions: vec![] },
            Request::DestroyInstance { instance: u64::MAX },
            Request::EnumPhysicalDevices { instance: 7 },
            Request::GetPhysicalDevice { pdev: 3, func: PdFunc::Properties2, arg: 0, count: 0, chain: vec![1000059001, 1000196000] },
            Request::GetPhysicalDevice { pdev: 3, func: PdFunc::FormatProperties, arg: 37, count: 0, chain: vec![] },
            Request::GetPhysicalDevice { pdev: 3, func: PdFunc::QueueFamilyProperties2, arg: 0, count: 4, chain: vec![1000059005] },
            Request::EnumDeviceExtensions { pdev: 9 },
        ]
    }

    fn all_responses() -> Vec<Response> {
        vec![
            Response::HelloOk { version: PROTOCOL_VERSION },
            Response::Handle(42),
            Response::Handles(vec![1, 2, u64::MAX]),
            Response::Handles(vec![]),
            Response::U32(0x0040_3000),
            Response::Data(vec![1, 2, 3, 0, 255]),
            Response::Data(vec![]),
            Response::Error { code: -3, message: "VK_ERROR_INCOMPATIBLE_DRIVER".into() },
        ]
    }

    #[test]
    fn requests_roundtrip() {
        for r in all_requests() {
            assert_eq!(Request::decode(&r.encode()).unwrap(), r, "{r:?}");
        }
    }

    #[test]
    fn responses_roundtrip() {
        for r in all_responses() {
            assert_eq!(Response::decode(&r.encode()).unwrap(), r, "{r:?}");
        }
    }

    #[test]
    fn first_byte_is_the_operation_code_and_hello_is_zero() {
        assert_eq!(Request::Hello { version: 1, token: vec![] }.encode()[0], 0);
        let ops: Vec<u8> = all_requests().iter().map(|r| r.encode()[0]).collect();
        let mut distinct = ops.clone();
        distinct.sort();
        distinct.dedup();
        assert!(distinct.len() >= 8, "każdy rodzaj żądania ma własny kod operacji: {ops:?}");
    }

    #[test]
    fn pdfunc_from_u8_covers_exactly_the_defined_values() {
        for v in 1..=10u8 {
            assert_eq!(PdFunc::from_u8(v).map(|f| f as u8), Some(v));
        }
        assert_eq!(PdFunc::from_u8(0), None);
        assert_eq!(PdFunc::from_u8(11), None);
        assert_eq!(PdFunc::from_u8(255), None);
    }

    #[test]
    fn truncated_messages_are_errors_never_panics() {
        for r in all_requests() {
            let b = r.encode();
            for cut in 0..b.len() {
                assert!(Request::decode(&b[..cut]).is_err(), "żądanie ucięte do {cut}/{}: {r:?}", b.len());
            }
        }
        for r in all_responses() {
            let b = r.encode();
            for cut in 0..b.len() {
                assert!(Response::decode(&b[..cut]).is_err(), "odpowiedź ucięta do {cut}/{}: {r:?}", b.len());
            }
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        for r in all_requests() {
            let mut b = r.encode();
            b.push(0);
            assert_eq!(Request::decode(&b), Err(MsgError::TrailingBytes(1)), "{r:?}");
        }
        let mut b = Response::U32(1).encode();
        b.extend([9, 9]);
        assert_eq!(Response::decode(&b), Err(MsgError::TrailingBytes(2)));
    }

    #[test]
    fn unknown_operation_codes_are_rejected() {
        assert_eq!(Request::decode(&[200]), Err(MsgError::UnknownOp(200)));
        assert_eq!(Response::decode(&[200]), Err(MsgError::UnknownOp(200)));
        assert!(Request::decode(&[]).is_err());
        assert!(Response::decode(&[]).is_err());
    }

    #[test]
    fn unknown_pdfunc_in_get_physical_device_is_rejected() {
        let mut b = Request::GetPhysicalDevice { pdev: 1, func: PdFunc::Properties, arg: 0, count: 0, chain: vec![] }.encode();
        // Bajt funkcji leży po kodzie operacji i 8 bajtach uchwytu.
        b[1 + 8] = 99;
        assert_eq!(Request::decode(&b), Err(MsgError::UnknownFunc(99)));
    }

    #[test]
    fn limits_are_enforced_on_decode() {
        // Za długi token.
        let long_token = Request::Hello { version: 1, token: vec![7; MAX_TOKEN_LEN + 1] }.encode();
        assert!(matches!(Request::decode(&long_token), Err(MsgError::LimitExceeded(_))));
        // Za dużo rozszerzeń.
        let many = Request::CreateInstance { api_version: 1, extensions: vec!["x".into(); MAX_EXTENSIONS + 1] }.encode();
        assert!(matches!(Request::decode(&many), Err(MsgError::LimitExceeded(_))));
        // Za długa nazwa.
        let name = Request::CreateInstance { api_version: 1, extensions: vec!["a".repeat(MAX_NAME_LEN + 1)] }.encode();
        assert!(matches!(Request::decode(&name), Err(MsgError::LimitExceeded(_))));
        // Za długi łańcuch.
        let chain = Request::GetPhysicalDevice { pdev: 1, func: PdFunc::Features2, arg: 0, count: 0, chain: vec![1; MAX_CHAIN + 1] }.encode();
        assert!(matches!(Request::decode(&chain), Err(MsgError::LimitExceeded(_))));
        // Za duża liczba elementów.
        let count = Request::GetPhysicalDevice { pdev: 1, func: PdFunc::QueueFamilyProperties, arg: 0, count: MAX_COUNT + 1, chain: vec![] }.encode();
        assert!(matches!(Request::decode(&count), Err(MsgError::LimitExceeded(_))));
        // Granice dokładnie na limicie są dozwolone.
        let ok = Request::CreateInstance { api_version: 1, extensions: vec!["a".repeat(MAX_NAME_LEN); MAX_EXTENSIONS] }.encode();
        assert!(Request::decode(&ok).is_ok());
    }

    #[test]
    fn hostile_length_prefixes_do_not_allocate_or_panic() {
        // Hello z tokenem o długości u64::MAX (varint) i brakiem danych.
        let mut e = gpu_wire::Encoder::new();
        e.put_u8(0);
        e.put_u32(1);
        e.put_varint(u64::MAX);
        assert!(Request::decode(&e.finish()).is_err());
        // Handles z liczbą elementów u64::MAX.
        let mut e = gpu_wire::Encoder::new();
        e.put_u8(2);
        e.put_varint(u64::MAX);
        assert!(Response::decode(&e.finish()).is_err());
        // Data z długością większą niż MAX_DATA.
        let mut e = gpu_wire::Encoder::new();
        e.put_u8(4);
        e.put_varint(MAX_DATA as u64 + 1);
        assert!(Response::decode(&e.finish()).is_err());
    }

    #[test]
    fn fuzz_random_bytes_never_panic() {
        let mut x: u64 = 0x1234_5678_9ABC_DEF1;
        let mut next = || {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        for _ in 0..4000 {
            let len = (next() % 80) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = Request::decode(&bytes);
            let _ = Response::decode(&bytes);
        }
    }
}
