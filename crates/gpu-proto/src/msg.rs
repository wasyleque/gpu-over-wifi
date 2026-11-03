//! Komunikaty wywołań M1 (zapytania o instancję i kartę): żądania klienta i odpowiedzi serwera.
//!
//! Kodowanie: pierwszy bajt = kod operacji (u8), potem pola przez `gpu_wire::Encoder`
//! (liczby LE, napisy i bajty z prefiksem długości). Dane z sieci są niezaufane:
//! dekoder sprawdza limity i odrzuca bajty nadmiarowe po poprawnym komunikacie.

use gpu_wire::{Decoder, Encoder, WireError};

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
/// Maksymalny rozmiar jednej partii poleceń / jednego zapisu pamięci (M2).
/// Najwięcej zakresów w jednym `MemReadDeltaRanges`.
pub const MAX_DELTA_RANGES: usize = 4096;
pub const MAX_BATCH: usize = 8 * 1024 * 1024;

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
        Some(match v {
            1 => PdFunc::Properties,
            2 => PdFunc::Properties2,
            3 => PdFunc::Features,
            4 => PdFunc::Features2,
            5 => PdFunc::MemoryProperties,
            6 => PdFunc::MemoryProperties2,
            7 => PdFunc::FormatProperties,
            8 => PdFunc::FormatProperties2,
            9 => PdFunc::QueueFamilyProperties,
            10 => PdFunc::QueueFamilyProperties2,
            _ => return None,
        })
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
    /// Partia poleceń Vulkana (kod operacji u16 + parametry kodeka), wykonywana bez odpowiedzi.
    Cmds { data: Vec<u8> },
    /// Jedno polecenie z parametrami wyjściowymi: odpowiedź `Data` niesie wyniki.
    Query { data: Vec<u8> },
    /// Punkt synchronizacji: odpowiedź `Synced` po wykonaniu wszystkiego, co przyszło wcześniej.
    Sync,
    /// Zapis do pamięci serwera (bez odpowiedzi).
    MemWrite { memory: u64, offset: u64, data: Vec<u8> },
    /// Jak `MemWrite`, ale `data` to blok LZ4, który po rozpakowaniu ma dokładnie `raw_len` bajtów (limit `MAX_BATCH`).
    MemWriteLz4 { memory: u64, offset: u64, raw_len: u32, data: Vec<u8> },
    /// Odczyt pamięci serwera.
    MemRead { memory: u64, offset: u64, len: u32 },
    /// Odczyt różnic: serwer odsyła tylko strony pamięci zmienione od ostatniego zapisu/odczytu (format: `pagediff::encode_segments`).
    MemReadDelta { memory: u64 },
    /// Jak `MemReadDelta`, ale serwer porównuje tylko wskazane zakresy bajtów (`(offset, długość)`, najwyżej `MAX_DELTA_RANGES`):
    /// tam, gdzie GPU może pisać; reszta pamięci jest pomijana, więc koszt nie zależy od jej rozmiaru.
    MemReadDeltaRanges { memory: u64, ranges: Vec<(u64, u64)> },
    /// Identyfikator tej sesji (do podłączenia drugiego połączenia z wideo).
    SessionId,
    /// Drugie połączenie (po `Hello`): odtąd serwer wypycha na nie ramki obrazu sesji `session`.
    AttachVideo { session: u64 },
    /// Bez odpowiedzi: serwer czyta `len` bajtów obrazu z `memory` (po wykonaniu wcześniejszych poleceń)
    /// i wysyła je jako ramkę `seq` na połączeniu wideo. `format`: 1 = BGRA8, 2 = RGBA8 (jak `video::PixelFormat`);
    /// `codec`: 0 = surowe, 2 = LZ4 (jak `video::Codec`).
    PresentFrame { memory: u64, len: u32, seq: u32, width: u16, height: u16, format: u8, codec: u8 },
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
    /// Koniec `Sync`: pierwszy asynchroniczny błąd od poprzedniego `Sync` (0 = brak).
    Synced { error: i32 },
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
        let mut e = Encoder::new();
        match self {
            Request::Hello { version, token } => {
                e.put_u8(0);
                e.put_u32(*version);
                e.put_bytes(token);
            }
            Request::EnumInstanceVersion => e.put_u8(1),
            Request::EnumInstanceExtensions => e.put_u8(2),
            Request::CreateInstance { api_version, extensions } => {
                e.put_u8(3);
                e.put_u32(*api_version);
                e.put_varint(extensions.len() as u64);
                for x in extensions {
                    e.put_str(x);
                }
            }
            Request::DestroyInstance { instance } => {
                e.put_u8(4);
                e.put_u64(*instance);
            }
            Request::EnumPhysicalDevices { instance } => {
                e.put_u8(5);
                e.put_u64(*instance);
            }
            Request::GetPhysicalDevice { pdev, func, arg, count, chain } => {
                e.put_u8(6);
                e.put_u64(*pdev);
                e.put_u8(*func as u8);
                e.put_u32(*arg);
                e.put_u32(*count);
                e.put_varint(chain.len() as u64);
                for c in chain {
                    e.put_u32(*c);
                }
            }
            Request::EnumDeviceExtensions { pdev } => {
                e.put_u8(7);
                e.put_u64(*pdev);
            }
            Request::Cmds { data } => {
                e.put_u8(8);
                e.put_bytes(data);
            }
            Request::Query { data } => {
                e.put_u8(9);
                e.put_bytes(data);
            }
            Request::Sync => e.put_u8(10),
            Request::MemWrite { memory, offset, data } => {
                e.put_u8(11);
                e.put_u64(*memory);
                e.put_u64(*offset);
                e.put_bytes(data);
            }
            Request::MemRead { memory, offset, len } => {
                e.put_u8(12);
                e.put_u64(*memory);
                e.put_u64(*offset);
                e.put_u32(*len);
            }
            Request::MemWriteLz4 { memory, offset, raw_len, data } => {
                e.put_u8(17);
                e.put_u64(*memory);
                e.put_u64(*offset);
                e.put_u32(*raw_len);
                e.put_bytes(data);
            }
            Request::MemReadDeltaRanges { memory, ranges } => {
                e.put_u8(18);
                e.put_u64(*memory);
                e.put_u32(ranges.len() as u32);
                for (off, len) in ranges {
                    e.put_u64(*off);
                    e.put_u64(*len);
                }
            }
            Request::MemReadDelta { memory } => {
                e.put_u8(16);
                e.put_u64(*memory);
            }
            Request::SessionId => e.put_u8(13),
            Request::AttachVideo { session } => {
                e.put_u8(14);
                e.put_u64(*session);
            }
            Request::PresentFrame { memory, len, seq, width, height, format, codec } => {
                e.put_u8(15);
                e.put_u64(*memory);
                e.put_u32(*len);
                e.put_u32(*seq);
                e.put_u16(*width);
                e.put_u16(*height);
                e.put_u8(*format);
                e.put_u8(*codec);
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Request, MsgError> {
        let mut d = Decoder::new(bytes);
        let req = match d.get_u8()? {
            0 => {
                let version = d.get_u32()?;
                let token = d.get_bytes()?;
                if token.len() > MAX_TOKEN_LEN {
                    return Err(MsgError::LimitExceeded("token"));
                }
                Request::Hello { version, token: token.to_vec() }
            }
            1 => Request::EnumInstanceVersion,
            2 => Request::EnumInstanceExtensions,
            3 => {
                let api_version = d.get_u32()?;
                let n = d.get_varint()?;
                if n > MAX_EXTENSIONS as u64 {
                    return Err(MsgError::LimitExceeded("extensions"));
                }
                let mut extensions = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let name = d.get_str()?;
                    if name.len() > MAX_NAME_LEN {
                        return Err(MsgError::LimitExceeded("extension name"));
                    }
                    extensions.push(name.to_string());
                }
                Request::CreateInstance { api_version, extensions }
            }
            4 => Request::DestroyInstance { instance: d.get_u64()? },
            5 => Request::EnumPhysicalDevices { instance: d.get_u64()? },
            6 => {
                let pdev = d.get_u64()?;
                let f = d.get_u8()?;
                let func = PdFunc::from_u8(f).ok_or(MsgError::UnknownFunc(f))?;
                let arg = d.get_u32()?;
                let count = d.get_u32()?;
                if count > MAX_COUNT {
                    return Err(MsgError::LimitExceeded("count"));
                }
                let n = d.get_varint()?;
                if n > MAX_CHAIN as u64 {
                    return Err(MsgError::LimitExceeded("chain"));
                }
                let mut chain = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    chain.push(d.get_u32()?);
                }
                Request::GetPhysicalDevice { pdev, func, arg, count, chain }
            }
            7 => Request::EnumDeviceExtensions { pdev: d.get_u64()? },
            8 | 9 => {
                let op = bytes.first().copied().unwrap_or(0);
                let data = d.get_bytes()?;
                if data.len() > MAX_BATCH {
                    return Err(MsgError::LimitExceeded("batch"));
                }
                if op == 8 {
                    Request::Cmds { data: data.to_vec() }
                } else {
                    Request::Query { data: data.to_vec() }
                }
            }
            10 => Request::Sync,
            11 => {
                let memory = d.get_u64()?;
                let offset = d.get_u64()?;
                let data = d.get_bytes()?;
                if data.len() > MAX_BATCH {
                    return Err(MsgError::LimitExceeded("memory write"));
                }
                Request::MemWrite { memory, offset, data: data.to_vec() }
            }
            12 => {
                let memory = d.get_u64()?;
                let offset = d.get_u64()?;
                let len = d.get_u32()?;
                if len as usize > MAX_BATCH {
                    return Err(MsgError::LimitExceeded("memory read"));
                }
                Request::MemRead { memory, offset, len }
            }
            13 => Request::SessionId,
            16 => Request::MemReadDelta { memory: d.get_u64()? },
            18 => {
                let memory = d.get_u64()?;
                let n = d.get_u32()? as usize;
                if n > MAX_DELTA_RANGES {
                    return Err(MsgError::LimitExceeded("delta ranges"));
                }
                let mut ranges = Vec::with_capacity(n);
                for _ in 0..n {
                    ranges.push((d.get_u64()?, d.get_u64()?));
                }
                Request::MemReadDeltaRanges { memory, ranges }
            }
            17 => {
                let memory = d.get_u64()?;
                let offset = d.get_u64()?;
                let raw_len = d.get_u32()?;
                let data = d.get_bytes()?;
                if raw_len as usize > MAX_BATCH || data.len() > MAX_BATCH {
                    return Err(MsgError::LimitExceeded("compressed memory write"));
                }
                Request::MemWriteLz4 { memory, offset, raw_len, data: data.to_vec() }
            }
            14 => Request::AttachVideo { session: d.get_u64()? },
            15 => {
                let memory = d.get_u64()?;
                let len = d.get_u32()?;
                let seq = d.get_u32()?;
                let width = d.get_u16()?;
                let height = d.get_u16()?;
                let format = d.get_u8()?;
                let codec = d.get_u8()?;
                if len as usize > MAX_BATCH {
                    return Err(MsgError::LimitExceeded("frame"));
                }
                Request::PresentFrame { memory, len, seq, width, height, format, codec }
            }
            op => return Err(MsgError::UnknownOp(op)),
        };
        match d.remaining() {
            0 => Ok(req),
            n => Err(MsgError::TrailingBytes(n)),
        }
    }
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match self {
            Response::HelloOk { version } => {
                e.put_u8(0);
                e.put_u32(*version);
            }
            Response::Handle(h) => {
                e.put_u8(1);
                e.put_u64(*h);
            }
            Response::Handles(hs) => {
                e.put_u8(2);
                e.put_varint(hs.len() as u64);
                for h in hs {
                    e.put_u64(*h);
                }
            }
            Response::U32(v) => {
                e.put_u8(3);
                e.put_u32(*v);
            }
            Response::Data(b) => {
                e.put_u8(4);
                e.put_bytes(b);
            }
            Response::Synced { error } => {
                e.put_u8(6);
                e.put_u32(*error as u32);
            }
            Response::Error { code, message } => {
                e.put_u8(5);
                e.put_u32(*code as u32);
                e.put_str(message);
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Response, MsgError> {
        let mut d = Decoder::new(bytes);
        let resp = match d.get_u8()? {
            0 => Response::HelloOk { version: d.get_u32()? },
            1 => Response::Handle(d.get_u64()?),
            2 => {
                let n = d.get_varint()?;
                // Każdy uchwyt zajmuje 8 bajtów: liczba większa niż zmieści bufor to błąd, bez alokacji.
                if n > MAX_COUNT as u64 || n > (d.remaining() / 8) as u64 {
                    return Err(MsgError::Wire(WireError::UnexpectedEof));
                }
                let mut hs = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    hs.push(d.get_u64()?);
                }
                Response::Handles(hs)
            }
            3 => Response::U32(d.get_u32()?),
            6 => Response::Synced { error: d.get_u32()? as i32 },
            4 => {
                let b = d.get_bytes()?;
                if b.len() > MAX_BATCH {
                    return Err(MsgError::LimitExceeded("data"));
                }
                Response::Data(b.to_vec())
            }
            5 => {
                let code = d.get_u32()? as i32;
                let message = d.get_str()?;
                if message.len() > MAX_NAME_LEN * 4 {
                    return Err(MsgError::LimitExceeded("message"));
                }
                Response::Error { code, message: message.to_string() }
            }
            op => return Err(MsgError::UnknownOp(op)),
        };
        match d.remaining() {
            0 => Ok(resp),
            n => Err(MsgError::TrailingBytes(n)),
        }
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
            Request::Cmds { data: vec![1, 2, 3] },
            Request::Cmds { data: vec![] },
            Request::Query { data: vec![9; 40] },
            Request::Sync,
            Request::MemWrite { memory: 5, offset: 4096, data: vec![0xAA; 100] },
            Request::MemRead { memory: 5, offset: 0, len: 4096 },
            Request::MemReadDelta { memory: 9 },
            Request::MemReadDeltaRanges { memory: 9, ranges: vec![(0, 4096), (1 << 40, 17)] },
            Request::MemReadDeltaRanges { memory: 9, ranges: vec![] },
            Request::MemWriteLz4 { memory: 5, offset: 8192, raw_len: 4096, data: vec![1, 2, 3] },
            Request::SessionId,
            Request::AttachVideo { session: u64::MAX },
            Request::PresentFrame { memory: 9, len: 1_870_000, seq: 77, width: 650, height: 718, format: 1, codec: 2 },
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
            Response::Synced { error: 0 },
            Response::Synced { error: -4 },
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
        assert!(distinct.len() >= 16, "każdy rodzaj żądania ma własny kod operacji: {ops:?}");
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
        let big = Request::Cmds { data: vec![0; MAX_BATCH + 1] }.encode();
        assert!(matches!(Request::decode(&big), Err(MsgError::LimitExceeded(_))));
        let bigw = Request::MemWrite { memory: 1, offset: 0, data: vec![0; MAX_BATCH + 1] }.encode();
        assert!(matches!(Request::decode(&bigw), Err(MsgError::LimitExceeded(_))));
        let bigr = Request::MemRead { memory: 1, offset: 0, len: MAX_BATCH as u32 + 1 }.encode();
        assert!(matches!(Request::decode(&bigr), Err(MsgError::LimitExceeded(_))));
        let bigf = Request::PresentFrame { memory: 1, len: MAX_BATCH as u32 + 1, seq: 0, width: 1, height: 1, format: 1, codec: 0 }.encode();
        assert!(matches!(Request::decode(&bigf), Err(MsgError::LimitExceeded(_))));
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
