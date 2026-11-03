//! Logika sesji: uwierzytelnianie, własność uchwytów i mapowanie żądań na `GpuBackend`.
//! Nie zależy od Vulkana (backend jest cechą), więc da się ją w pełni testować atrapą.

use gpu_proto::msg::{PdFunc, Request, Response, MAX_NAME_LEN, PROTOCOL_VERSION};
use gpu_wire::{Decoder, Encoder};

/// `VK_ERROR_UNKNOWN`: kod dla odrzuconych żądań (nieznany uchwyt, limit).
pub const ERR_UNKNOWN: i32 = -13;
/// `VK_ERROR_INITIALIZATION_FAILED`.
pub const ERR_INIT_FAILED: i32 = -3;
/// Kod 0 w `Response::Error` oznacza błąd protokołu (nie `VkResult`).
pub const ERR_PROTOCOL: i32 = 0;
/// Maksymalna liczba jednocześnie żywych instancji w jednej sesji.
pub const MAX_INSTANCES: usize = 16;

/// Backend karty graficznej. Błąd to ujemny `VkResult`.
pub trait GpuBackend {
    fn instance_version(&mut self) -> Result<u32, i32>;
    fn instance_extensions(&mut self) -> Result<Vec<(String, u32)>, i32>;
    fn create_instance(&mut self, api_version: u32, extensions: &[String]) -> Result<u64, i32>;
    fn destroy_instance(&mut self, instance: u64) -> Result<(), i32>;
    fn enum_physical_devices(&mut self, instance: u64) -> Result<Vec<u64>, i32>;
    fn get_physical_device(
        &mut self,
        pdev: u64,
        func: PdFunc,
        arg: u32,
        count: u32,
        chain: &[u32],
    ) -> Result<Vec<u8>, i32>;
    fn device_extensions(&mut self, pdev: u64) -> Result<Vec<(String, u32)>, i32>;

    /// Wykonuje partię poleceń (M2). Domyślnie nieobsługiwane.
    fn execute(&mut self, _batch: &[u8]) -> Result<(), i32> {
        Err(ERR_UNKNOWN)
    }
    /// Wykonuje pojedyncze polecenie z wynikami wyjściowymi i zwraca zakodowane wyniki.
    fn query(&mut self, _data: &[u8]) -> Result<Vec<u8>, i32> {
        Err(ERR_UNKNOWN)
    }
    fn mem_write(&mut self, _memory: u64, _offset: u64, _data: &[u8]) -> Result<(), i32> {
        Err(ERR_UNKNOWN)
    }
    fn mem_read(&mut self, _memory: u64, _offset: u64, _len: u32) -> Result<Vec<u8>, i32> {
        Err(ERR_UNKNOWN)
    }
    /// Odczyt samych różnic pamięci (patrz `Request::MemReadDelta`); `max_bytes` ogranicza jedną odpowiedź.
    fn mem_read_delta(&mut self, _memory: u64, _max_bytes: usize) -> Result<Vec<u8>, i32> {
        Err(ERR_UNKNOWN)
    }
    /// Czeka, aż wszystko wysłane wcześniej zostanie wykonane po stronie backendu.
    fn sync(&mut self) -> Result<(), i32> {
        Ok(())
    }
}

/// Kodowanie listy rozszerzeń w `Response::Data`: varint liczby, potem (napis, u32 specVersion).
pub fn encode_extensions(list: &[(String, u32)]) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.put_varint(list.len() as u64);
    for (name, spec) in list {
        enc.put_str(name);
        enc.put_u32(*spec);
    }
    enc.finish()
}

/// Odwrotność `encode_extensions`; `None` przy błędnych danych (ucięte, za długie nazwy, nadmiar).
pub fn decode_extensions(bytes: &[u8]) -> Option<Vec<(String, u32)>> {
    let mut dec = Decoder::new(bytes);
    let count = match dec.get_varint() {
        Ok(c) if c <= 1024 && c <= (bytes.len() as u64) => c as usize,
        _ => return None,
    };
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        let name = match dec.get_str() {
            Ok(s) if s.len() <= MAX_NAME_LEN => s.to_string(),
            _ => return None,
        };
        let spec = match dec.get_u32() {
            Ok(s) => s,
            _ => return None,
        };
        result.push((name, spec));
    }
    if dec.remaining() != 0 {
        None
    } else {
        Some(result)
    }
}

/// Jedna sesja klienta. Dopóki `Hello` nie przejdzie, wszystkie inne żądania są odrzucane.
/// Czas spędzony w obsłudze żądań (µs i liczba), według rodzaju; `GPU_RENDERD_STATS=1` wypisuje to co 5 s.
/// Pozwala odróżnić koszt wykonania poleceń, kopiowania klatki i kodowania od czekania na sieć.
mod acct {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    pub const NAMES: [&str; 8] = ["cmds", "memwrite", "query", "memreaddelta", "sync", "present_read", "present_encode", "present_send"];
    struct State {
        us: [u64; 8],
        n: [u64; 8],
        last: Option<Instant>,
    }
    static S: Mutex<State> = Mutex::new(State { us: [0; 8], n: [0; 8], last: None });
    pub fn enabled() -> bool {
        static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *E.get_or_init(|| std::env::var_os("GPU_RENDERD_STATS").is_some())
    }
    pub fn add(kind: usize, d: Duration) {
        let mut s = S.lock().unwrap_or_else(|e| e.into_inner());
        s.us[kind] += d.as_micros() as u64;
        s.n[kind] += 1;
        let now = Instant::now();
        if s.last.is_none_or(|t| now.duration_since(t).as_secs() >= 5) {
            if s.last.is_some() {
                let line: Vec<String> = (0..8).filter(|&k| s.n[k] > 0).map(|k| format!("{} {}x {}ms", NAMES[k], s.n[k], s.us[k] / 1000)).collect();
                eprintln!("gpu-renderd czas (od startu): {}", line.join(", "));
            }
            s.last = Some(now);
        }
    }
}

pub struct Session<B: GpuBackend> {
    backend: B,
    token: Vec<u8>,
    authed: bool,
    /// instancja -> fizyczne karty zwrócone przez `EnumPhysicalDevices` (tylko takie wolno odpytywać).
    instances: Vec<(u64, Vec<u64>)>,
    /// Pierwszy błąd z poleceń bez odpowiedzi od ostatniego `Sync` (0 = brak).
    async_error: i32,
    /// Identyfikator sesji i nadajnik ramek obrazu na jej połączenie wideo.
    session_id: u64,
    video_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// Koder H.264 bieżącego kanału wideo (tworzony przy pierwszej klatce H.264, zerowany przy nowym `SessionId`).
    encoder: Option<gpu_nvenc::encoder::H264Encoder>,
}

impl<B: GpuBackend> Session<B> {
    pub fn new(backend: B, token: Vec<u8>) -> Self {
        let (session_id, video_tx) = crate::videohub::new_session();
        Session {
            session_id,
            video_tx,
            encoder: None,
            backend,
            token,
            authed: false,
            instances: Vec::new(),
            async_error: 0,
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.authed
    }

    /// Jak `handle`, ale `None` dla poleceń bez odpowiedzi (`Cmds`, `MemWrite`) wykonanych poprawnie.
    pub fn handle_request(&mut self, req: Request) -> Option<Response> {
        let silent = self.authed && matches!(req, Request::Cmds { .. } | Request::MemWrite { .. } | Request::PresentFrame { .. });
        let r = self.handle(req);
        if silent && !matches!(r, Response::Error { .. }) {
            None
        } else {
            Some(r)
        }
    }

    /// Czyta obraz z pamięci urządzenia, koduje (surowo albo LZ4) i wypycha go jako ramkę na połączenie wideo.
    #[allow(clippy::too_many_arguments)]
    fn present_frame(&mut self, memory: u64, len: u32, seq: u32, width: u16, height: u16, format: u8, codec: u8) -> Result<(), i32> {
        use gpu_proto::video::{compress_lz4, Codec, PixelFormat, VideoHeader};
        let format = match format {
            1 => PixelFormat::Bgra8,
            2 => PixelFormat::Rgba8,
            _ => return Err(ERR_UNKNOWN),
        };
        let codec = match codec {
            0 => Codec::Raw,
            2 => Codec::Lz4,
            3 => Codec::H264,
            _ => return Err(ERR_UNKNOWN),
        };
        // Surowy rozmiar musi odpowiadać wymiarom (nagłówek waliduje to tylko dla `Raw`).
        let expected = (width as u64) * (height as u64) * 4;
        if width == 0 || height == 0 || len as u64 != expected {
            return Err(ERR_UNKNOWN);
        }
        let t = acct::enabled().then(std::time::Instant::now);
        let data = self.backend.mem_read(memory, 0, len)?;
        if let Some(t) = t {
            acct::add(5, t.elapsed());
        }
        if data.len() != len as usize {
            return Err(ERR_UNKNOWN);
        }
        let t = acct::enabled().then(std::time::Instant::now);
        let (codec, body) = match codec {
            Codec::H264 => match self.encode_h264(&data, width, height, format == PixelFormat::Rgba8) {
                Some(b) => (Codec::H264, b),
                // Brak NVENC albo wymiary poza zakresem kodera: ramka idzie bezstratnie (LZ4), odbiorca czyta kodek z nagłówka.
                None => (Codec::Lz4, compress_lz4(&data)),
            },
            Codec::Lz4 => (Codec::Lz4, compress_lz4(&data)),
            c => (c, data),
        };
        if let Some(t) = t {
            acct::add(6, t.elapsed());
        }
        let header = VideoHeader { swapchain: 0, image_index: 0, seq, width, height, format, codec, payload_len: body.len() as u32 };
        // Nagłówek sam się waliduje: niepoprawna ramka nie wychodzi z serwera.
        let hb = header.encode();
        VideoHeader::decode(&hb).map_err(|_| ERR_UNKNOWN)?;
        let mut payload = Vec::with_capacity(hb.len() + body.len());
        payload.extend_from_slice(&hb);
        payload.extend_from_slice(&body);
        // Blokuje, gdy odbiorca nie nadąża (backpressure); brak odbiorcy (odłączone wideo) nie jest błędem sesji.
        let t = acct::enabled().then(std::time::Instant::now);
        let _ = self.video_tx.send(payload);
        if let Some(t) = t {
            acct::add(7, t.elapsed());
        }
        Ok(())
    }

    /// Koduje klatkę kodekiem H.264 (koder powstaje przy pierwszej klatce i jest odtwarzany przy zmianie wymiarów lub formatu).
    fn encode_h264(&mut self, data: &[u8], width: u16, height: u16, rgba: bool) -> Option<Vec<u8>> {
        use gpu_nvenc::encoder::{EncoderConfig, H264Encoder};
        let mbps: u32 = std::env::var("GPU_RENDERD_H264_MBPS").ok().and_then(|v| v.parse().ok()).filter(|v| (1..=800).contains(v)).unwrap_or(50);
        let cfg = EncoderConfig { width: width as u32, height: height as u32, fps: 60, bitrate_bps: mbps * 1_000_000, rgba };
        if self.encoder.as_ref().map(|e| e.config()) != Some(cfg) {
            self.encoder = None;
            let nv = gpu_nvenc::api::Nvenc::load().map_err(|e| eprintln!("gpu-renderd: NVENC niedostępny: {e}")).ok()?;
            self.encoder = Some(H264Encoder::new(&nv, cfg).map_err(|e| eprintln!("gpu-renderd: koder H.264: {e}")).ok()?);
        }
        self.encoder.as_mut()?.encode(data, false).map_err(|e| eprintln!("gpu-renderd: kodowanie H.264: {e}")).ok()
    }

    /// Wykonuje `f` na backendzie i (przy `GPU_RENDERD_STATS`) zalicza czas do rodzaju `kind` (patrz `acct`).
    fn timed<T>(&mut self, kind: usize, f: impl FnOnce(&mut B) -> T) -> T {
        let t = acct::enabled().then(std::time::Instant::now);
        let r = f(&mut self.backend);
        if let Some(t) = t {
            acct::add(kind, t.elapsed());
        }
        r
    }

    /// Identyfikator sesji (do podłączenia połączenia wideo).
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    fn note_async(&mut self, r: Result<(), i32>) -> Response {
        if let Err(code) = r {
            if self.async_error == 0 {
                self.async_error = code;
            }
        }
        Response::U32(0)
    }

    pub fn handle(&mut self, req: Request) -> Response {
        if !self.authed {
            match req {
                Request::Hello { version, token } => {
                    if version != PROTOCOL_VERSION || token.is_empty() || token.len() > MAX_NAME_LEN
                    {
                        return Response::Error {
                            code: ERR_PROTOCOL,
                            message: "authentication failed".to_string(),
                        };
                    }
                    if self.token.len() != token.len() {
                        return Response::Error {
                            code: ERR_PROTOCOL,
                            message: "authentication failed".to_string(),
                        };
                    }
                    let equal = self
                        .token
                        .iter()
                        .zip(token.iter())
                        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                        == 0;
                    if !equal {
                        return Response::Error {
                            code: ERR_PROTOCOL,
                            message: "authentication failed".to_string(),
                        };
                    }
                    self.authed = true;
                    Response::HelloOk { version }
                }
                _ => Response::Error {
                    code: ERR_PROTOCOL,
                    message: "not authenticated".to_string(),
                },
            }
        } else {
            match req {
                Request::Hello { .. } => Response::Error {
                    code: ERR_PROTOCOL,
                    message: "already authenticated".to_string(),
                },
                Request::SessionId => {
                    // Każde zapytanie tworzy świeży kanał wideo (nowy swapchain): poprzedni zamyka się razem z jego połączeniem.
                    let (id, tx) = crate::videohub::new_session();
                    crate::videohub::forget(self.session_id);
                    self.session_id = id;
                    self.video_tx = tx;
                    self.encoder = None; // nowy strumień zaczyna się od IDR
                    Response::Handle(id)
                }
                Request::AttachVideo { .. } => Response::Error { code: ERR_PROTOCOL, message: "attach only as the first request of a video connection".to_string() },
                Request::PresentFrame { memory, len, seq, width, height, format, codec } => {
                    let r = self.present_frame(memory, len, seq, width, height, format, codec);
                    self.note_async(r)
                }
                Request::Cmds { data } => {
                    let t = acct::enabled().then(std::time::Instant::now);
                    let r = self.backend.execute(&data);
                    if let Some(t) = t {
                        acct::add(0, t.elapsed());
                    }
                    self.note_async(r)
                }
                Request::MemWrite { memory, offset, data } => {
                    let t = acct::enabled().then(std::time::Instant::now);
                    let r = self.backend.mem_write(memory, offset, &data);
                    if let Some(t) = t {
                        acct::add(1, t.elapsed());
                    }
                    self.note_async(r)
                }
                Request::Query { data } => match self.timed(2, |b| b.query(&data)) {
                    Ok(v) => Response::Data(v),
                    Err(code) => Response::Error { code, message: "query error".to_string() },
                },
                Request::MemRead { memory, offset, len } => match self.backend.mem_read(memory, offset, len) {
                    Ok(v) => Response::Data(v),
                    Err(code) => Response::Error { code, message: "memory read error".to_string() },
                },
                Request::MemReadDelta { memory } => match self.timed(3, |b| b.mem_read_delta(memory, gpu_proto::msg::MAX_BATCH - 64)) {
                    Ok(v) => Response::Data(v),
                    Err(code) => Response::Error { code, message: "memory delta read error".to_string() },
                },
                Request::Sync => {
                    let t = acct::enabled().then(std::time::Instant::now);
                    let r = self.backend.sync();
                    if let Some(t) = t {
                        acct::add(4, t.elapsed());
                    }
                    self.note_async(r);
                    Response::Synced { error: std::mem::take(&mut self.async_error) }
                }
                Request::EnumInstanceVersion => match self.backend.instance_version() {
                    Ok(v) => Response::U32(v),
                    Err(code) => Response::Error {
                        code,
                        message: "instance version error".to_string(),
                    },
                },
                Request::EnumInstanceExtensions => match self.backend.instance_extensions() {
                    Ok(exts) => Response::Data(encode_extensions(&exts)),
                    Err(code) => Response::Error {
                        code,
                        message: "instance extensions error".to_string(),
                    },
                },
                Request::CreateInstance {
                    api_version,
                    extensions,
                } => {
                    if self.instances.len() >= MAX_INSTANCES {
                        return Response::Error {
                            code: ERR_UNKNOWN,
                            message: "too many instances".to_string(),
                        };
                    }
                    match self.backend.create_instance(api_version, &extensions) {
                        Ok(handle) => {
                            self.instances.push((handle, Vec::new()));
                            Response::Handle(handle)
                        }
                        Err(code) => Response::Error {
                            code,
                            message: "create instance error".to_string(),
                        },
                    }
                }
                Request::DestroyInstance { instance } => {
                    let idx = self.instances.iter().position(|(h, _)| *h == instance);
                    match idx {
                        Some(i) => {
                            let (handle, _) = self.instances.remove(i);
                            match self.backend.destroy_instance(handle) {
                                Ok(_) => Response::U32(0),
                                Err(code) => Response::Error {
                                    code,
                                    message: "destroy instance error".to_string(),
                                },
                            }
                        }
                        None => Response::Error {
                            code: ERR_UNKNOWN,
                            message: "unknown instance".to_string(),
                        },
                    }
                }
                Request::EnumPhysicalDevices { instance } => {
                    let idx = self.instances.iter().position(|(h, _)| *h == instance);
                    match idx {
                        Some(i) => {
                            let (handle, ref mut devices) = self.instances[i];
                            if devices.is_empty() {
                                match self.backend.enum_physical_devices(handle) {
                                    Ok(d) => {
                                        *devices = d;
                                        Response::Handles(devices.clone())
                                    }
                                    Err(code) => Response::Error {
                                        code,
                                        message: "enum physical devices error".to_string(),
                                    },
                                }
                            } else {
                                Response::Handles(devices.clone())
                            }
                        }
                        None => Response::Error {
                            code: ERR_UNKNOWN,
                            message: "unknown instance".to_string(),
                        },
                    }
                }
                Request::GetPhysicalDevice {
                    pdev,
                    func,
                    arg,
                    count,
                    chain,
                } => {
                    let mut found = false;
                    for (_, devices) in &self.instances {
                        if devices.contains(&pdev) {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        return Response::Error {
                            code: ERR_UNKNOWN,
                            message: "unknown physical device".to_string(),
                        };
                    }
                    match self
                        .backend
                        .get_physical_device(pdev, func, arg, count, &chain)
                    {
                        Ok(data) => Response::Data(data),
                        Err(code) => Response::Error {
                            code,
                            message: "get physical device error".to_string(),
                        },
                    }
                }
                Request::EnumDeviceExtensions { pdev } => {
                    let mut found = false;
                    for (_, devices) in &self.instances {
                        if devices.contains(&pdev) {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        return Response::Error {
                            code: ERR_UNKNOWN,
                            message: "unknown physical device".to_string(),
                        };
                    }
                    match self.backend.device_extensions(pdev) {
                        Ok(exts) => Response::Data(encode_extensions(&exts)),
                        Err(code) => Response::Error {
                            code,
                            message: "device extensions error".to_string(),
                        },
                    }
                }
            }
        }
    }

    /// Wywoływane po rozłączeniu: niszczy wszystkie instancje, które sesja jeszcze trzyma.
    pub fn cleanup(&mut self) {
        crate::videohub::forget(self.session_id);
        for (handle, _) in self.instances.drain(..) {
            let _ = self.backend.destroy_instance(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Calls {
        log: Vec<String>,
        next_handle: u64,
    }

    #[derive(Clone, Default)]
    struct Mock {
        calls: Rc<RefCell<Calls>>,
        fail_create: bool,
    }

    impl GpuBackend for Mock {
        fn instance_version(&mut self) -> Result<u32, i32> {
            self.calls.borrow_mut().log.push("version".into());
            Ok(0x0040_3000)
        }
        fn instance_extensions(&mut self) -> Result<Vec<(String, u32)>, i32> {
            Ok(vec![
                ("VK_KHR_surface".into(), 25),
                ("VK_EXT_debug_utils".into(), 2),
            ])
        }
        fn create_instance(&mut self, api: u32, ext: &[String]) -> Result<u64, i32> {
            if self.fail_create {
                return Err(-9);
            }
            let mut c = self.calls.borrow_mut();
            c.next_handle += 100;
            let h = c.next_handle;
            c.log
                .push(format!("create {api:#x} {} -> {h}", ext.join(",")));
            Ok(h)
        }
        fn destroy_instance(&mut self, i: u64) -> Result<(), i32> {
            self.calls.borrow_mut().log.push(format!("destroy {i}"));
            Ok(())
        }
        fn enum_physical_devices(&mut self, i: u64) -> Result<Vec<u64>, i32> {
            Ok(vec![i + 1, i + 2])
        }
        fn get_physical_device(
            &mut self,
            p: u64,
            f: PdFunc,
            arg: u32,
            count: u32,
            chain: &[u32],
        ) -> Result<Vec<u8>, i32> {
            self.calls
                .borrow_mut()
                .log
                .push(format!("get {p} {f:?} {arg} {count} {chain:?}"));
            Ok(vec![0xAB; 4])
        }
        fn device_extensions(&mut self, _p: u64) -> Result<Vec<(String, u32)>, i32> {
            Ok(vec![("VK_KHR_swapchain".into(), 70)])
        }
    }

    fn session() -> (Session<Mock>, Rc<RefCell<Calls>>) {
        let m = Mock::default();
        let calls = m.calls.clone();
        (Session::new(m, b"sekret".to_vec()), calls)
    }

    fn hello(token: &[u8]) -> Request {
        Request::Hello {
            version: PROTOCOL_VERSION,
            token: token.to_vec(),
        }
    }

    fn authed() -> (Session<Mock>, Rc<RefCell<Calls>>) {
        let (mut s, c) = session();
        assert_eq!(
            s.handle(hello(b"sekret")),
            Response::HelloOk {
                version: PROTOCOL_VERSION
            }
        );
        (s, c)
    }

    fn is_err(r: &Response) -> bool {
        matches!(r, Response::Error { .. })
    }

    #[test]
    fn extensions_encoding_roundtrips_and_rejects_garbage() {
        let l = vec![("VK_KHR_surface".to_string(), 25), ("x".to_string(), 0)];
        assert_eq!(decode_extensions(&encode_extensions(&l)), Some(l));
        assert_eq!(decode_extensions(&encode_extensions(&[])), Some(vec![]));
        let b = encode_extensions(&[("VK_KHR_surface".into(), 25)]);
        for cut in 0..b.len() {
            assert_eq!(decode_extensions(&b[..cut]), None, "ucięte do {cut}");
        }
        let mut extra = b.clone();
        extra.push(0);
        assert_eq!(decode_extensions(&extra), None, "nadmiarowy bajt");
        let long = encode_extensions(&[("a".repeat(MAX_NAME_LEN + 1), 1)]);
        assert_eq!(decode_extensions(&long), None, "za długa nazwa");
        let mut e = Encoder::new();
        e.put_varint(u64::MAX);
        assert_eq!(decode_extensions(&e.finish()), None, "wrogi licznik");
    }

    #[test]
    fn nothing_works_before_hello_and_backend_is_untouched() {
        let (mut s, calls) = session();
        for r in [
            Request::EnumInstanceVersion,
            Request::EnumInstanceExtensions,
            Request::CreateInstance {
                api_version: 1,
                extensions: vec![],
            },
            Request::EnumPhysicalDevices { instance: 1 },
            Request::DestroyInstance { instance: 1 },
            Request::EnumDeviceExtensions { pdev: 1 },
        ] {
            assert!(is_err(&s.handle(r.clone())), "{r:?}");
        }
        assert!(calls.borrow().log.is_empty(), "{:?}", calls.borrow().log);
        assert!(!s.is_authenticated());
    }

    #[test]
    fn wrong_token_or_version_fails_and_stays_unauthenticated() {
        let (mut s, _) = session();
        assert!(is_err(&s.handle(hello(b"zly"))));
        assert!(is_err(&s.handle(hello(b""))));
        assert!(is_err(&s.handle(hello(b"sekret1"))), "token dłuższy o znak");
        assert!(is_err(&s.handle(hello(b"sekre"))), "token krótszy o znak");
        assert!(is_err(&s.handle(Request::Hello {
            version: PROTOCOL_VERSION + 1,
            token: b"sekret".to_vec()
        })));
        assert!(!s.is_authenticated());
        assert!(is_err(&s.handle(Request::EnumInstanceVersion)));
        assert_eq!(
            s.handle(hello(b"sekret")),
            Response::HelloOk {
                version: PROTOCOL_VERSION
            },
            "poprawny Hello po błędnych"
        );
        assert!(s.is_authenticated());
    }

    #[test]
    fn empty_configured_token_never_authenticates() {
        let mut s = Session::new(Mock::default(), Vec::new());
        assert!(
            is_err(&s.handle(hello(b""))),
            "pusty token w konfiguracji nie może wpuszczać"
        );
    }

    #[test]
    fn different_tokens_never_authenticate_even_when_xor_folds_to_zero() {
        // Wzorzec `acc ^ a ^ b` dałby tu 0: (1^2)=3, potem 3^3^0=0.
        let mut s = Session::new(Mock::default(), vec![1, 3]);
        assert!(is_err(&s.handle(Request::Hello {
            version: PROTOCOL_VERSION,
            token: vec![2, 0]
        })));
        assert!(!s.is_authenticated());
        let mut s = Session::new(Mock::default(), vec![1, 3]);
        assert!(!is_err(&s.handle(Request::Hello {
            version: PROTOCOL_VERSION,
            token: vec![1, 3]
        })));
    }

    #[test]
    fn second_hello_after_success_is_an_error() {
        let (mut s, _) = authed();
        assert!(is_err(&s.handle(hello(b"sekret"))));
    }

    #[test]
    fn version_and_extension_queries_after_hello() {
        let (mut s, _) = authed();
        assert_eq!(
            s.handle(Request::EnumInstanceVersion),
            Response::U32(0x0040_3000)
        );
        let Response::Data(d) = s.handle(Request::EnumInstanceExtensions) else {
            panic!()
        };
        assert_eq!(
            decode_extensions(&d).unwrap()[0],
            ("VK_KHR_surface".to_string(), 25)
        );
    }

    #[test]
    fn instance_lifecycle_and_device_queries() {
        let (mut s, calls) = authed();
        let Response::Handle(inst) = s.handle(Request::CreateInstance {
            api_version: 0x0040_3000,
            extensions: vec!["VK_KHR_surface".into()],
        }) else {
            panic!()
        };
        let Response::Handles(pds) = s.handle(Request::EnumPhysicalDevices { instance: inst })
        else {
            panic!()
        };
        assert_eq!(pds, vec![inst + 1, inst + 2]);
        let r = s.handle(Request::GetPhysicalDevice {
            pdev: pds[0],
            func: PdFunc::Properties2,
            arg: 0,
            count: 0,
            chain: vec![1000059001],
        });
        assert_eq!(r, Response::Data(vec![0xAB; 4]));
        assert!(calls
            .borrow()
            .log
            .iter()
            .any(|l| l.contains("get") && l.contains("Properties2") && l.contains("1000059001")));
        let Response::Data(d) = s.handle(Request::EnumDeviceExtensions { pdev: pds[1] }) else {
            panic!()
        };
        assert_eq!(decode_extensions(&d).unwrap()[0].0, "VK_KHR_swapchain");
        assert_eq!(
            s.handle(Request::DestroyInstance { instance: inst }),
            Response::U32(0)
        );
        assert!(calls.borrow().log.contains(&format!("destroy {inst}")));
    }

    #[test]
    fn unknown_handles_are_rejected_before_reaching_the_backend() {
        let (mut s, calls) = authed();
        assert!(is_err(
            &s.handle(Request::EnumPhysicalDevices { instance: 12345 })
        ));
        assert!(is_err(
            &s.handle(Request::DestroyInstance { instance: 12345 })
        ));
        assert!(is_err(&s.handle(Request::GetPhysicalDevice {
            pdev: 777,
            func: PdFunc::Properties,
            arg: 0,
            count: 0,
            chain: vec![]
        })));
        assert!(is_err(
            &s.handle(Request::EnumDeviceExtensions { pdev: 777 })
        ));
        assert!(
            !calls
                .borrow()
                .log
                .iter()
                .any(|l| l.starts_with("get") || l.starts_with("destroy")),
            "{:?}",
            calls.borrow().log
        );
    }

    #[test]
    fn physical_device_not_yet_enumerated_is_rejected() {
        let (mut s, _) = authed();
        let Response::Handle(inst) = s.handle(Request::CreateInstance {
            api_version: 1,
            extensions: vec![],
        }) else {
            panic!()
        };
        // Karta inst+1 zostałaby zwrócona, ale jeszcze nie wyliczono.
        assert!(is_err(&s.handle(Request::GetPhysicalDevice {
            pdev: inst + 1,
            func: PdFunc::Properties,
            arg: 0,
            count: 0,
            chain: vec![]
        })));
    }

    #[test]
    fn destroying_an_instance_revokes_its_physical_devices_and_double_destroy_fails() {
        let (mut s, _) = authed();
        let Response::Handle(inst) = s.handle(Request::CreateInstance {
            api_version: 1,
            extensions: vec![],
        }) else {
            panic!()
        };
        let Response::Handles(pds) = s.handle(Request::EnumPhysicalDevices { instance: inst })
        else {
            panic!()
        };
        assert!(!is_err(
            &s.handle(Request::DestroyInstance { instance: inst })
        ));
        assert!(is_err(&s.handle(Request::GetPhysicalDevice {
            pdev: pds[0],
            func: PdFunc::Properties,
            arg: 0,
            count: 0,
            chain: vec![]
        })));
        assert!(is_err(
            &s.handle(Request::DestroyInstance { instance: inst })
        ));
    }

    #[test]
    fn repeated_enumeration_does_not_duplicate_or_break_access() {
        let (mut s, _) = authed();
        let Response::Handle(inst) = s.handle(Request::CreateInstance {
            api_version: 1,
            extensions: vec![],
        }) else {
            panic!()
        };
        let a = s.handle(Request::EnumPhysicalDevices { instance: inst });
        let b = s.handle(Request::EnumPhysicalDevices { instance: inst });
        assert_eq!(a, b);
        assert!(!is_err(
            &s.handle(Request::EnumDeviceExtensions { pdev: inst + 1 })
        ));
    }

    #[derive(Default)]
    struct Stream {
        batches: Vec<Vec<u8>>,
        fail_on: Option<u8>,
    }

    impl GpuBackend for Stream {
        fn instance_version(&mut self) -> Result<u32, i32> { Ok(0) }
        fn instance_extensions(&mut self) -> Result<Vec<(String, u32)>, i32> { Ok(vec![]) }
        fn create_instance(&mut self, _: u32, _: &[String]) -> Result<u64, i32> { Ok(1) }
        fn destroy_instance(&mut self, _: u64) -> Result<(), i32> { Ok(()) }
        fn enum_physical_devices(&mut self, _: u64) -> Result<Vec<u64>, i32> { Ok(vec![]) }
        fn get_physical_device(&mut self, _: u64, _: PdFunc, _: u32, _: u32, _: &[u32]) -> Result<Vec<u8>, i32> { Ok(vec![]) }
        fn device_extensions(&mut self, _: u64) -> Result<Vec<(String, u32)>, i32> { Ok(vec![]) }
        fn execute(&mut self, b: &[u8]) -> Result<(), i32> {
            self.batches.push(b.to_vec());
            if b.first().copied() == self.fail_on && self.fail_on.is_some() { Err(-4) } else { Ok(()) }
        }
        fn query(&mut self, d: &[u8]) -> Result<Vec<u8>, i32> { Ok(d.iter().rev().copied().collect()) }
        fn mem_read(&mut self, m: u64, o: u64, l: u32) -> Result<Vec<u8>, i32> { if o != 0 { Ok(vec![m as u8, o as u8, l as u8]) } else { Ok(vec![m as u8; l as usize]) } }
        fn mem_write(&mut self, _: u64, _: u64, d: &[u8]) -> Result<(), i32> { if d.is_empty() { Err(-5) } else { Ok(()) } }
    }

    fn stream_session(fail_on: Option<u8>) -> Session<Stream> {
        let mut s = Session::new(Stream { fail_on, ..Default::default() }, b"t".to_vec());
        assert!(s.handle_request(Request::Hello { version: PROTOCOL_VERSION, token: b"t".to_vec() }).is_some());
        s
    }

    #[test]
    fn cmds_and_memwrite_are_silent_and_sync_reports_the_first_async_error() {
        let mut s = stream_session(Some(2));
        assert_eq!(s.handle_request(Request::Cmds { data: vec![1] }), None);
        assert_eq!(s.handle_request(Request::Cmds { data: vec![2] }), None, "błąd nie psuje ciszy");
        assert_eq!(s.handle_request(Request::Cmds { data: vec![3] }), None);
        assert_eq!(s.handle_request(Request::MemWrite { memory: 1, offset: 0, data: vec![] }), None);
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: -4 }), "pierwszy błąd (-4), nie drugi (-5)");
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: 0 }), "Sync czyści błąd");
    }

    #[test]
    fn query_and_memread_reply_and_unauthenticated_cmds_get_an_error_reply() {
        let mut s = stream_session(None);
        assert_eq!(s.handle_request(Request::Query { data: vec![1, 2, 3] }), Some(Response::Data(vec![3, 2, 1])));
        assert_eq!(s.handle_request(Request::MemRead { memory: 7, offset: 8, len: 9 }), Some(Response::Data(vec![7, 8, 9])));
        let mut u = Session::new(Stream::default(), b"t".to_vec());
        assert!(matches!(u.handle_request(Request::Cmds { data: vec![1] }), Some(Response::Error { .. })));
        assert!(u.backend.batches.is_empty(), "przed Hello nic nie dociera do backendu");
    }

    #[test]
    fn present_frame_is_pushed_to_the_video_channel_with_a_valid_header() {
        use gpu_proto::video::{Codec, PixelFormat, VideoHeader};
        let mut s = stream_session(None);
        let Some(Response::Handle(id)) = s.handle_request(Request::SessionId) else { panic!() };
        assert_eq!(id, s.session_id());
        let rx = crate::videohub::claim(id).unwrap();
        assert_eq!(s.handle_request(Request::PresentFrame { memory: 7, len: 16, seq: 5, width: 2, height: 2, format: 1, codec: 0 }), None);
        let payload = rx.try_recv().unwrap();
        let h = VideoHeader::decode(&payload[..28]).unwrap();
        assert_eq!((h.seq, h.width, h.height, h.format, h.codec, h.payload_len), (5, 2, 2, PixelFormat::Bgra8, Codec::Raw, 16));
        assert_eq!(&payload[28..], &[7u8; 16][..]);
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: 0 }));
    }

    #[test]
    fn every_session_id_request_opens_a_fresh_video_channel_and_closes_the_previous_one() {
        let mut s = stream_session(None);
        let Some(Response::Handle(a)) = s.handle_request(Request::SessionId) else { panic!() };
        let rx_a = crate::videohub::claim(a).unwrap();
        let Some(Response::Handle(b)) = s.handle_request(Request::SessionId) else { panic!() };
        assert_ne!(a, b);
        assert!(rx_a.recv().is_err(), "stary kanał jest zamknięty, więc jego połączenie wideo się kończy");
        let rx_b = crate::videohub::claim(b).unwrap();
        s.handle_request(Request::PresentFrame { memory: 7, len: 4, seq: 1, width: 1, height: 1, format: 2, codec: 0 });
        assert!(rx_b.try_recv().is_ok(), "ramki idą nowym kanałem");
    }

    #[test]
    fn present_frame_with_inconsistent_geometry_or_format_is_an_async_error_and_sends_nothing() {
        let mut s = stream_session(None);
        let Some(Response::Handle(id)) = s.handle_request(Request::SessionId) else { panic!() };
        let rx = crate::videohub::claim(id).unwrap();
        assert_eq!(s.handle_request(Request::PresentFrame { memory: 7, len: 15, seq: 0, width: 2, height: 2, format: 1, codec: 0 }), None);
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: ERR_UNKNOWN }));
        assert_eq!(s.handle_request(Request::PresentFrame { memory: 7, len: 16, seq: 0, width: 2, height: 2, format: 9, codec: 0 }), None);
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: ERR_UNKNOWN }));
        assert_eq!(s.handle_request(Request::PresentFrame { memory: 7, len: 16, seq: 0, width: 0, height: 2, format: 1, codec: 0 }), None);
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: ERR_UNKNOWN }));
        assert!(rx.try_recv().is_err(), "żadna niepoprawna ramka nie wyszła");
    }

    #[test]
    fn lz4_frames_are_compressed_and_rejected_when_the_codec_or_size_is_wrong() {
        use gpu_proto::video::{decompress_lz4, Codec, VideoHeader};
        let mut s = stream_session(None);
        let Some(Response::Handle(id)) = s.handle_request(Request::SessionId) else { panic!() };
        let rx = crate::videohub::claim(id).unwrap();
        assert_eq!(s.handle_request(Request::PresentFrame { memory: 7, len: 64 * 4, seq: 1, width: 8, height: 8, format: 1, codec: 2 }), None);
        let payload = rx.try_recv().unwrap();
        let h = VideoHeader::decode(&payload[..28]).unwrap();
        assert_eq!((h.codec, h.payload_len as usize), (Codec::Lz4, payload.len() - 28));
        assert!(payload.len() - 28 < 64 * 4, "płaski obraz się kompresuje");
        assert_eq!(decompress_lz4(&payload[28..], 64 * 4).unwrap(), vec![7u8; 64 * 4]);
        s.handle_request(Request::PresentFrame { memory: 7, len: 64 * 4, seq: 2, width: 8, height: 8, format: 1, codec: 1 });
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: ERR_UNKNOWN }), "HEVC nie jest jeszcze obsługiwany");
        s.handle_request(Request::PresentFrame { memory: 7, len: 100, seq: 3, width: 8, height: 8, format: 1, codec: 2 });
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: ERR_UNKNOWN }), "len różne od w*h*4");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn h264_request_for_a_frame_the_encoder_cannot_take_falls_back_to_lz4() {
        use gpu_proto::video::{decompress_lz4, Codec, VideoHeader};
        let mut s = stream_session(None);
        let Some(Response::Handle(id)) = s.handle_request(Request::SessionId) else { panic!() };
        let rx = crate::videohub::claim(id).unwrap();
        // 8x8 jest poniżej minimum NVENC (33), więc niezależnie od maszyny koder odmawia i ramka idzie jako LZ4.
        assert_eq!(s.handle_request(Request::PresentFrame { memory: 7, len: 64 * 4, seq: 1, width: 8, height: 8, format: 1, codec: 3 }), None);
        let payload = rx.try_recv().unwrap();
        let h = VideoHeader::decode(&payload[..28]).unwrap();
        assert_eq!(h.codec, Codec::Lz4);
        assert_eq!(decompress_lz4(&payload[28..], 64 * 4).unwrap(), vec![7u8; 64 * 4]);
    }

    #[test]
    fn unauthenticated_present_frame_and_attach_are_rejected() {
        let mut u = Session::new(Stream::default(), b"t".to_vec());
        assert!(matches!(u.handle_request(Request::PresentFrame { memory: 1, len: 4, seq: 0, width: 1, height: 1, format: 1, codec: 0 }), Some(Response::Error { .. })));
        let mut a = stream_session(None);
        assert!(matches!(a.handle_request(Request::AttachVideo { session: 1 }), Some(Response::Error { .. })), "attach działa tylko na poziomie połączenia");
    }

    #[test]
    fn default_backend_rejects_m2_requests_with_an_error() {
        let (mut s, _) = authed();
        assert_eq!(s.handle_request(Request::Cmds { data: vec![1] }), None);
        assert_eq!(s.handle_request(Request::Sync), Some(Response::Synced { error: ERR_UNKNOWN }));
        assert!(matches!(s.handle_request(Request::Query { data: vec![] }), Some(Response::Error { .. })));
    }

    #[test]
    fn instance_limit_is_enforced_and_freed_by_destroy() {
        let (mut s, _) = authed();
        let mut handles = Vec::new();
        for _ in 0..MAX_INSTANCES {
            let Response::Handle(h) = s.handle(Request::CreateInstance {
                api_version: 1,
                extensions: vec![],
            }) else {
                panic!()
            };
            handles.push(h);
        }
        assert!(is_err(&s.handle(Request::CreateInstance {
            api_version: 1,
            extensions: vec![]
        })));
        s.handle(Request::DestroyInstance {
            instance: handles[0],
        });
        assert!(!is_err(&s.handle(Request::CreateInstance {
            api_version: 1,
            extensions: vec![]
        })));
    }

    #[test]
    fn backend_error_is_passed_through_with_its_code() {
        let m = Mock {
            fail_create: true,
            ..Default::default()
        };
        let mut s = Session::new(m, b"t".to_vec());
        s.handle(Request::Hello {
            version: PROTOCOL_VERSION,
            token: b"t".to_vec(),
        });
        match s.handle(Request::CreateInstance {
            api_version: 1,
            extensions: vec![],
        }) {
            Response::Error { code, .. } => assert_eq!(code, -9),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cleanup_destroys_all_remaining_instances_once() {
        let (mut s, calls) = authed();
        let Response::Handle(a) = s.handle(Request::CreateInstance {
            api_version: 1,
            extensions: vec![],
        }) else {
            panic!()
        };
        let Response::Handle(b) = s.handle(Request::CreateInstance {
            api_version: 1,
            extensions: vec![],
        }) else {
            panic!()
        };
        s.handle(Request::DestroyInstance { instance: a });
        s.cleanup();
        s.cleanup();
        let destroys: Vec<_> = calls
            .borrow()
            .log
            .iter()
            .filter(|l| l.starts_with("destroy"))
            .cloned()
            .collect();
        assert_eq!(
            destroys,
            vec![format!("destroy {a}"), format!("destroy {b}")]
        );
    }
}
