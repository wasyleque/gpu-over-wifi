//! Połączenie sterujące z serwerem z rozdzielonym zapisem i odczytem odpowiedzi.
//!
//! Serwer odpowiada na żądania w kolejności ich wysłania, a po stronie klienta zapisy idą pod blokadą instancji (kolejność w strumieniu).
//! Wcześniej wątek czekał na odpowiedź TRZYMAJĄC blokadę instancji, więc każde oczekiwanie (fence, semafor, zapytanie) zatrzymywało
//! wszystkie pozostałe wątki gry na czas obiegu po sieci (w Cyberpunku 250 s oczekiwania na blokadę przy 100 s jej trzymania).
//! Tu każde żądanie z odpowiedzią dostaje bilet (kolejny numer) w chwili wysłania, a odpowiedzi odbiera się w kolejności biletów bez
//! blokady instancji: wątek o bilecie `k` czeka, aż odczytano odpowiedzi `0..k`, czyta swoją i przekazuje turę dalej.

use std::collections::VecDeque;
use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex};

use gpu_proto::frame::{flags, Frame, FrameReader};
use gpu_proto::msg::{Request, Response};
use gpu_renderd::client::{Client, ClientError};

/// Czy serwer odpowie na to żądanie (`Cmds`, `MemWrite*` i `PresentFrame` są ciche, o ile się powiodą).
pub fn expects_reply(req: &Request) -> bool {
    !matches!(req, Request::Cmds { .. } | Request::MemWrite { .. } | Request::MemWriteLz4 { .. } | Request::MemWriteZstd { .. } | Request::MemWriteZstdDict { .. } | Request::MemWriteZstdPfx { .. } | Request::MemWriteRunsPfx { .. } | Request::MemWriteRuns { .. } | Request::PresentFrame { .. })
}

struct Turn {
    /// Ile odpowiedzi już odczytano (numer biletu, który jest teraz na swojej kolei).
    served: u64,
    /// Odczyt zawiódł (rozłączenie, błąd ramki): wszyscy oczekujący dostają błąd zamiast czekać w nieskończoność.
    broken: bool,
    /// Bilety odpowiedzi, na które nikt nie czeka (oczekiwanie zwrócone grze wcześniej, patrz `Shared::defer`): `(bilet, rodzaj)`, rodzaj 0 = odpowiedź na
    /// zapytanie (pierwsze 4 bajty to kod wyniku), 1 = odpowiedź `Sync`. Odczytuje je następny wątek, który czeka na swoją odpowiedź.
    deferred: Vec<(u64, u8)>,
    /// Jakiś wątek właśnie odczytuje odroczoną odpowiedź.
    draining: bool,
}

/// Pierwszy zapamiętany błąd z odroczonej odpowiedzi (0 = brak), do zgłoszenia przy następnym wywołaniu synchronicznym.
pub static DEFERRED_FAIL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
pub static DEFERRED_DONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Reader {
    stream: TcpStream,
    frames: FrameReader,
}

/// Część dzielona między wątkami: odczyt odpowiedzi w kolejności biletów.
pub struct Shared {
    turn: Mutex<Turn>,
    cv: Condvar,
    reader: Mutex<Reader>,
}

impl Shared {
    /// Rejestruje odpowiedzi `ticket` (zapytanie) i `ticket + 1` (`Sync`) jako odroczone: wołający nie czeka na nie, odczyta je kolejny czekający wątek.
    pub fn defer(&self, ticket: u64) {
        let mut t = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        t.deferred.push((ticket, 0));
        t.deferred.push((ticket + 1, 1));
    }

    /// Obrabia odroczoną odpowiedź: kod wyniku różny od 0 albo błąd serwera zostaje zapamiętany w `DEFERRED_FAIL` (zgłoszony przy następnym wywołaniu).
    fn finish_deferred(kind: u8, ticket: u64, r: &Result<Response, ClientError>) {
        use std::sync::atomic::Ordering::Relaxed;
        if kind == 0 {
            crate::fwd::prefetch_reply(ticket, r);
        }
        DEFERRED_DONE.fetch_add(1, Relaxed);
        let code = match r {
            Ok(Response::Data(d)) if kind == 0 => d.get(..4).map_or(0, |b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            // Odpowiedź `Sync`: błąd asynchroniczny wcześniejszych poleceń (serwer zeruje go po zgłoszeniu, więc nie wolno go zgubić).
            Ok(Response::Synced { error }) => *error,
            Ok(_) => 0,
            Err(ClientError::Remote(c, _)) => *c,
            Err(_) => -4, // VK_ERROR_DEVICE_LOST: połączenie zerwane
        };
        if code != 0 && DEFERRED_FAIL.compare_exchange(0, code, Relaxed, Relaxed).is_ok() {
            eprintln!("gpu-icd: ODROCZONA odpowiedź serwera z kodem {code} (rodzaj {kind}); następne wywołanie synchroniczne zgłosi błąd");
        }
    }

    /// Odbiera odpowiedź dla biletu `ticket` (czeka na swoją kolej; nie wymaga blokady instancji). Odroczone odpowiedzi wcześniejszych biletów odczytuje po drodze.
    pub fn recv_at(&self, ticket: u64) -> Result<Response, ClientError> {
        let t_enter = std::time::Instant::now();
        loop {
            let mut t = self.turn.lock().unwrap_or_else(|e| e.into_inner());
            if t.broken {
                return Err(ClientError::Closed);
            }
            if t.served == ticket {
                break;
            }
            let served = t.served;
            let kind = t.deferred.iter().find(|d| d.0 == served).map(|d| d.1);
            match kind {
                Some(kind) if !t.draining => {
                    t.draining = true;
                    drop(t);
                    let r = self.read_one();
                    Self::finish_deferred(kind, served, &r);
                    let mut t = self.turn.lock().unwrap_or_else(|e| e.into_inner());
                    t.deferred.retain(|d| d.0 != served);
                    t.served += 1;
                    t.draining = false;
                    if matches!(r, Err(ClientError::Io(_) | ClientError::Closed | ClientError::Frame(_) | ClientError::Msg(_))) {
                        t.broken = true;
                    }
                    self.cv.notify_all();
                }
                _ => {
                    let _t = self.cv.wait(t).unwrap_or_else(|e| e.into_inner());
                }
            }
        }
        // Tylko wątek, którego jest kolej, dochodzi tutaj, więc blokada odczytu nie jest rywalizowana.
        let t_turn = std::time::Instant::now();
        TURN_WAIT_NS.fetch_add((t_turn - t_enter).as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        let r = self.read_one();
        READ_ONE_NS.fetch_add(t_turn.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        RECV_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        marks_replied(ticket);
        let mut t = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        t.served += 1;
        if matches!(r, Err(ClientError::Io(_) | ClientError::Closed | ClientError::Frame(_) | ClientError::Msg(_))) {
            t.broken = true;
        }
        self.cv.notify_all();
        r
    }

    fn read_one(&self) -> Result<Response, ClientError> {
        use std::io::Read;
        let mut rd = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = [0u8; 65536];
        loop {
            if let Some(f) = rd.frames.next_frame().map_err(ClientError::Frame)? {
                return match Response::decode(&f.payload).map_err(ClientError::Msg)? {
                    Response::Error { code, message } => Err(ClientError::Remote(code, message)),
                    r => Ok(r),
                };
            }
            let n = rd.stream.read(&mut buf)?;
            if n == 0 {
                return Err(ClientError::Closed);
            }
            rd.frames.push(&buf[..n]);
        }
    }
}

/// Kolejka bajtów do wysłania: wątek nadawczy zapisuje do gniazda, a wątki gry tylko dokładają (bez czekania na przepustowość łącza).
struct Out {
    q: Mutex<OutState>,
    /// Sygnał dla wątku nadawczego: są dane albo zamknięcie.
    data: Condvar,
    /// Sygnał dla producentów: kolejka się zmniejszyła.
    space: Condvar,
    /// Zapisy pamięci czekające na kompresję przez wątki robocze (kolejność w strumieniu wyznacza `q`, nie ta kolejka).
    work: Mutex<VecDeque<Arc<Job>>>,
    work_cv: Condvar,
    work_closed: std::sync::atomic::AtomicBool,
}

/// Zapis pamięci do skompresowania i opakowania w ramkę. Robi to wątek roboczy albo, gdy żaden jeszcze go nie wziął, wątek nadawczy w swojej kolei.
/// Wejście zadania: numer ramki, pamięć, przesunięcie, bajty.
type RunsInput = (u16, u64, Vec<(u64, u32)>, Vec<(u32, u64)>, Vec<u8>);

enum JobInput {
    Mem(u16, u64, u64, Vec<u8>),
    /// Zapis z prefiksem: numer ramki, pamięć, przesunięcie, bajty i poprzednia treść tego zakresu (kopia z lustra z chwili zakolejkowania).
    MemPfx(u16, u64, u64, Vec<u8>, MirrorTurn),
    /// Kilka fragmentów jednej pamięci: numer ramki, pamięć, fragmenty `(przesunięcie, długość)`, sklejone bajty i sklejone poprzednie treści (z lustra).
    RunsPfx(u16, u64, Vec<(u64, u32)>, Vec<u8>, MirrorTurn),
    Runs(RunsInput),
}
type JobOutput = Result<Vec<u8>, ()>;

/// Lustro "ostatnio przesłane w to miejsce" po pamięci (słownik zstd dla zapisów z prefiksem). Aktualizują je zadania zapisu w KOLEJNOŚCI numerów tur
/// (numer nadaje wątek wołający pod blokadą instancji, a samo kopiowanie robi wątek roboczy), dzięki czemu kopie bajtów nie wydłużają blokady instancji.
struct MirrorGate {
    st: Mutex<MirrorSt>,
    cv: Condvar,
    /// Ustawiane przy zamykaniu łącza: czekające zadania rezygnują zamiast czekać na turę, która nie nadejdzie.
    dead: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct MirrorSt {
    /// Numer następnej tury do wykonania.
    next: u64,
    map: std::collections::HashMap<u64, Vec<u8>>,
}

/// Tura zadania wobec lustra: numer, pamięci do zapomnienia przed tym zapisem i uchwyt do lustra.
struct MirrorTurn {
    turn: u64,
    forget: Vec<u64>,
    gate: Arc<MirrorGate>,
}

impl MirrorTurn {
    /// Czeka na swoją turę, zbiera prefiks (stare treści zakresów `runs` pamięci `memory`, sklejone), wpisuje do lustra nowe bajty `raw` i oddaje turę następnemu.
    fn take(self, memory: u64, runs: &[(u64, u32)], raw: &[u8]) -> Result<Vec<u8>, ()> {
        use std::sync::atomic::Ordering::Relaxed;
        let g = &self.gate;
        let mut st = g.st.lock().unwrap_or_else(|e| e.into_inner());
        while st.next != self.turn {
            if g.dead.load(Relaxed) {
                return Err(());
            }
            st = g.cv.wait_timeout(st, std::time::Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
        }
        for m in &self.forget {
            st.map.remove(m);
        }
        let t0 = std::time::Instant::now();
        let mut prefix = Vec::with_capacity(raw.len());
        let m = st.map.entry(memory).or_default();
        let mut at = 0usize;
        for &(o, l) in runs {
            let (off, end) = (o as usize, o as usize + l as usize);
            if m.len() < end {
                m.resize(end, 0);
            }
            prefix.extend_from_slice(&m[off..end]);
            m[off..end].copy_from_slice(&raw[at..at + l as usize]);
            at += l as usize;
        }
        GROUP_COPY_NS.fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
        st.next += 1;
        drop(st);
        g.cv.notify_all();
        Ok(prefix)
    }
}


struct Job {
    input: Mutex<Option<JobInput>>,
    output: Mutex<Option<JobOutput>>,
    done: Condvar,
    len: usize,
}

impl Job {
    fn run(&self) {
        let taken = self.input.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(input) = taken {
            let r = match input {
                JobInput::Mem(seq, memory, offset, raw) => mem_frame(seq, memory, offset, raw),
                JobInput::MemPfx(seq, memory, offset, raw, turn) => match turn.take(memory, &[(offset, raw.len() as u32)], &raw) {
                    Ok(prefix) => mem_frame_pfx(seq, memory, offset, raw, prefix),
                    Err(()) => Err(ClientError::Closed),
                },
                JobInput::RunsPfx(seq, memory, runs, raw, turn) => match turn.take(memory, &runs, &raw) {
                    Ok(prefix) => runs_pfx_frame(seq, memory, runs, raw, prefix),
                    Err(()) => Err(ClientError::Closed),
                },
                JobInput::Runs((seq, memory, runs, pages, raw)) => runs_frame(seq, memory, runs, pages, raw),
            }
            .map_err(|_| ());
            *self.output.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            self.done.notify_all();
        }
    }

    /// Wynik w kolejności strumienia: jeśli nikt jeszcze nie zaczął, robimy to sami; w przeciwnym razie czekamy na wątek roboczy.
    fn wait(&self) -> Result<Vec<u8>, ()> {
        self.run();
        let mut g = self.output.lock().unwrap_or_else(|e| e.into_inner());
        while g.is_none() {
            g = self.done.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.take().unwrap_or(Err(()))
    }
}

/// Element kolejki nadawczej: gotowe bajty albo zapis pamięci kompresowany poza blokadą instancji (LZ4 i CRC pod nią kosztowały 42 z 57 s jej
/// trzymania na 3 429 klatek Rise of the Tomb Raider). Kompresję robi kilka wątków roboczych równolegle, a wątek nadawczy wysyła w kolejności
/// kolejki (jeden wątek na kompresję i zapis naraz wydłużył czekanie na odpowiedzi serwera 2x w Black Book).
enum Item {
    Bytes(Vec<u8>),
    /// Bajty żądania z odpowiedzią: bilet i chwila zakolejkowania (diagnostyka: ile trwa kolejka+zapis, a ile serwer+powrót).
    Stamped(Vec<u8>, u64, std::time::Instant),
    Job(Arc<Job>),
}

impl Item {
    fn queued_len(&self) -> usize {
        match self {
            Item::Bytes(b) | Item::Stamped(b, ..) => b.len(),
            Item::Job(j) => j.len,
        }
    }
}

/// Diagnostyka czekania: dla każdego żądania z odpowiedzią czas od zakolejkowania do zapisu w gnieździe (kolejka za zapisami pamięci + wysyłka)
/// oraz od zapisu do odebrania odpowiedzi (łącze, serwer, powrót). Dotyczy pierwszego żądania z odpowiedzią w zapisie.
struct Mark {
    t0: std::time::Instant,
    written: Option<std::time::Instant>,
}
static MARKS: Mutex<Option<std::collections::HashMap<u64, Mark>>> = Mutex::new(None);
static MARK_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MARK_QUEUE_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MARK_REPLY_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn marks_written(ticket: u64, t0: std::time::Instant) {
    let now = std::time::Instant::now();
    let mut g = MARKS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(Default::default);
    if m.len() < 4096 {
        m.insert(ticket, Mark { t0, written: Some(now) });
    }
}

pub static TURN_WAIT_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static READ_ONE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static RECV_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn marks_replied(ticket: u64) {
    let now = std::time::Instant::now();
    let m = MARKS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.remove(&ticket));
    if let Some(Mark { t0, written: Some(w) }) = m {
        use std::sync::atomic::Ordering::Relaxed;
        MARK_N.fetch_add(1, Relaxed);
        MARK_QUEUE_US.fetch_add(w.duration_since(t0).as_micros() as u64, Relaxed);
        MARK_REPLY_US.fetch_add(now.duration_since(w).as_micros() as u64, Relaxed);
    }
}

/// Średnie z `Mark`: (liczba, kolejka+zapis w µs, zapis→odpowiedź w µs).
pub fn wait_split() -> (u64, u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    let n = MARK_N.load(Relaxed).max(1);
    (MARK_N.load(Relaxed), MARK_QUEUE_US.load(Relaxed) / n, MARK_REPLY_US.load(Relaxed) / n)
}

/// Czas (ns) kopiowania w `send_mem_group` pod blokadą instancji: prefiks i surowe bajty z lustra/shadow oraz aktualizacja lustra.
pub static GROUP_COPY_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static GROUP_MIRROR_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static WIRE_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WIRE_BY_MEM: Mutex<Option<std::collections::HashMap<u64, u64>>> = Mutex::new(None);

/// Bajty zapisów pamięci faktycznie wysłane łączem (po kompresji), łącznie i dla jednej pamięci (diagnostyka).
pub fn wire_total() -> u64 {
    WIRE_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn wire_of(memory: u64) -> u64 {
    WIRE_BY_MEM.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&memory).copied()).unwrap_or(0)
}

/// Czy zapisy pamięci idą kodowane zstd (poziom 1; na ruchu Tomb Raidera o ok. 17% mniej bajtów niż LZ4 przy 2-3x wolniejszym kodowaniu).
/// `GPU_ICD_MEM_LZ4=1` wraca do LZ4 (np. dla starszego serwera, który nie zna `MemWriteZstd`).
/// Poziom zstd dla zapisów pamięci (`GPU_ICD_ZSTD_LEVEL`, domyślnie 1; ujemne -1..-7 to tryby "fast": szybsze kodowanie kosztem rozmiaru, z prefiksem lustra): łącze 1 Gb/s jest w ciężkich scenach prawie nasycone, a wątki robocze mają zapas CPU.
fn zstd_level() -> i32 {
    static V: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("GPU_ICD_ZSTD_LEVEL").ok().and_then(|v| v.parse().ok()).filter(|l| (-7..=19).contains(l) && *l != 0).unwrap_or(1))
}

/// Tryb zapisów pamięci: start zawsze od zstd z prefiksem (o połowę mniej bajtów); gdy ruch na łączu jest przez dłuższy czas mały, a łącze szybkie, sesja
/// przechodzi JEDNOKIERUNKOWO na LZ4 (zapisy rozpakowuje wątek odczytu serwera, nie wątek wykonujący). W jedną stronę, bo po przejściu żadna ramka nie
/// używa prefiksu, więc lustra klienta i serwera nie muszą pozostać zgodne. Pomiar 2026-11-21 (łącze 2,5 Gb/s): Cyberpunk (ok. 25 MB/s zstd) +3% z LZ4;
/// Rise of the Tomb Raider (100-125 MB/s zstd, 200-250 MB/s LZ4) Overall 40,5 z zstd wobec 36,5 z LZ4. `GPU_ICD_MEM_LZ4=1` wymusza LZ4 od początku,
/// `=0` wymusza zstd bez przełączania.
static LZ4_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn env_codec() -> Option<bool> {
    static V: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("GPU_ICD_MEM_LZ4").ok().map(|v| v != "0"))
}

fn mem_zstd() -> bool {
    match env_codec() {
        Some(lz4) => !lz4,
        None => !LZ4_MODE.load(std::sync::atomic::Ordering::Relaxed),
    }
}

/// Decyzja o przejściu na LZ4: okna 5 s, ruch na łączu (MB/s) < `LOW_MBS` przez `NEED_WINDOWS` kolejnych okien z faktycznym ruchem (>= `ACTIVE_MBS`).
const ADAPT_WINDOW_S: f64 = 5.0;
const LOW_MBS: f64 = 60.0;
const ACTIVE_MBS: f64 = 5.0;
const NEED_WINDOWS: u32 = 6;

#[derive(Default)]
struct Adapt {
    win_start_s: f64,
    win_wire: u64,
    low: u32,
    started: bool,
}

impl Adapt {
    /// `now_s`: czas w sekundach od dowolnego początku, `wire`: skumulowane bajty na łączu. Zwraca `true`, gdy sesja ma przejść na LZ4.
    fn observe(&mut self, now_s: f64, wire: u64) -> bool {
        if !self.started {
            self.started = true;
            self.win_start_s = now_s;
            self.win_wire = wire;
            return false;
        }
        let dt = now_s - self.win_start_s;
        if dt < ADAPT_WINDOW_S {
            return false;
        }
        let mbs = wire.saturating_sub(self.win_wire) as f64 / dt / 1e6;
        self.win_start_s = now_s;
        self.win_wire = wire;
        if (ACTIVE_MBS..LOW_MBS).contains(&mbs) {
            self.low += 1;
        } else {
            self.low = 0;
        }
        self.low >= NEED_WINDOWS
    }
}

/// Wołane przy każdym zapisie pamięci (tanio: jedno porównanie czasu pod mutexem); po przejściu na LZ4 nic nie robi.
fn adapt_codec() {
    use std::sync::atomic::Ordering::Relaxed;
    if env_codec().is_some() || LZ4_MODE.load(Relaxed) || !link_fast() {
        return;
    }
    static ST: Mutex<Option<(std::time::Instant, Adapt)>> = Mutex::new(None);
    let mut g = ST.lock().unwrap_or_else(|e| e.into_inner());
    let (t0, ad) = g.get_or_insert_with(|| (std::time::Instant::now(), Adapt::default()));
    let now = t0.elapsed().as_secs_f64();
    if ad.observe(now, WIRE_TOTAL.load(Relaxed)) {
        LZ4_MODE.store(true, Relaxed);
        eprintln!("[gpu-icd] ruch zapisów pamięci mały i stały: przejście na LZ4 (jednokierunkowo)");
    }
}

/// Prędkość łącza do serwera z `GPU_ICD_ADDR` w Mb/s: interfejs z tablicy tras (najdłuższy pasujący prefiks), prędkość z `/sys/class/net/<if>/speed`.
/// `None`, gdy nie da się ustalić (Wi-Fi nie podaje prędkości, adres nie jest IPv4, brak trasy).
pub fn link_mbps() -> Option<u32> {
    let addr = std::env::var("GPU_ICD_ADDR").ok()?;
    let host = addr.rsplit_once(':').map_or(addr.as_str(), |(h, _)| h);
    let ip: std::net::Ipv4Addr = host.parse().ok()?;
    let iface = route_iface(&std::fs::read_to_string("/proc/net/route").ok()?, ip)?;
    let v: i64 = std::fs::read_to_string(format!("/sys/class/net/{iface}/speed")).ok()?.trim().parse().ok()?;
    (v > 0).then_some(v as u32)
}

/// Interfejs trasy do `ip` z tekstu `/proc/net/route` (najdłuższy pasujący prefiks). Pola adresu i maski to bajty sieciowe wypisane jako liczba
/// w porządku hosta, więc na little endian `10.42.0.0` jest `00002A0A`.
fn route_iface(route: &str, ip: std::net::Ipv4Addr) -> Option<String> {
    let ip = u32::from_le_bytes(ip.octets());
    let mut best: Option<(u32, String)> = None;
    for line in route.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 8 {
            continue;
        }
        let (Ok(dest), Ok(mask)) = (u32::from_str_radix(f[1], 16), u32::from_str_radix(f[7], 16)) else { continue };
        if ip & mask == dest {
            let bits = mask.count_ones();
            if best.as_ref().is_none_or(|(b, _)| bits > *b) {
                best = Some((bits, f[0].to_string()));
            }
        }
    }
    best.map(|(_, i)| i)
}

fn link_fast() -> bool {
    link_mbps().is_some_and(|m| m >= 2000)
}

/// Bajty czekające w kolejce wysyłki (aktualizuje wątek wysyłający i producenci).
static LINK_BACKLOG: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `GPU_ICD_RAW_BELOW=<MB>`: gdy kolejka wysyłki ma nie więcej niż tyle bajtów, łącze nadąża i zapisy pamięci idą bez kompresji (oszczędza CPU
/// klienta i serwera); gdy kolejka rośnie, wracają do kompresji. Domyślnie 0 = zawsze kompresuj (zachowanie bez zmian).
fn link_has_headroom() -> bool {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let below = *V.get_or_init(|| std::env::var("GPU_ICD_RAW_BELOW").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0) << 20);
    below > 0 && LINK_BACKLOG.load(std::sync::atomic::Ordering::Relaxed) <= below
}

/// `GPU_ICD_DICT=<plik .dict>`: zapisy pamięci kodowane słownikiem zstd (ten sam plik musi leżeć w katalogu `GPU_RENDERD_DICT_DIR` serwera). Zastępuje lustro/prefiks i LZ4.
fn mem_dict() -> Option<&'static (u32, Vec<u8>)> {
    static V: std::sync::OnceLock<Option<(u32, Vec<u8>)>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let path = std::env::var("GPU_ICD_DICT").ok()?;
        let bytes = std::fs::read(&path).map_err(|e| eprintln!("gpu-icd: słownik {path}: {e}")).ok()?;
        let id = zstd::zstd_safe::get_dict_id_from_dict(&bytes).map(|i| i.get());
        if id.is_none() {
            eprintln!("gpu-icd: {path} nie jest słownikiem zstd z identyfikatorem; ignoruję");
        }
        id.map(|i| (i, bytes))
    })
    .as_ref()
}

thread_local! {
    static DICT_COMP: std::cell::RefCell<Option<zstd::bulk::Compressor<'static>>> = const { std::cell::RefCell::new(None) };
}

/// Kompresja zstd (poziom 1) słownikiem klienta; `None`, gdy słownika nie ma albo kompresja się nie powiodła.
fn dict_compress(raw: &[u8]) -> Option<(u32, Vec<u8>)> {
    let (id, dict) = mem_dict()?;
    DICT_COMP.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = zstd::bulk::Compressor::with_dictionary(1, dict).ok();
        }
        c.as_mut()?.compress(raw).ok().map(|v| (*id, v))
    })
}

/// Zapis pamięci jako ramka: zstd (albo LZ4) tylko gdy oszczędza co najmniej 10% (małe i źle kompresowalne, np. tekstury BCn, idą surowo).
fn mem_frame(seq: u16, memory: u64, offset: u64, raw: Vec<u8>) -> Result<Vec<u8>, ClientError> {
    let raw_len = raw.len() as u32;
    if raw.len() >= 4096 && !link_has_headroom() {
        if let Some((dict_id, c)) = dict_compress(&raw).filter(|(_, c)| c.len() * 10 <= raw.len() * 9) {
            let n = c.len() as u64;
            WIRE_TOTAL.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            *WIRE_BY_MEM.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).entry(memory).or_insert(0) += n;
            let frame = Frame::new(0, flags::CRC, seq, 0, Request::MemWriteZstdDict { memory, offset, raw_len, dict_id, data: c }.encode()).map_err(ClientError::Frame)?;
            return Ok(frame.encode());
        }
    }
    let packed = (raw.len() >= 4096 && !link_has_headroom())
        .then(|| {
            if mem_zstd() {
                zstd::bulk::compress(&raw, 1).ok().map(|c| (true, c))
            } else {
                Some((false, gpu_proto::video::compress_lz4(&raw)))
            }
        })
        .flatten()
        .filter(|(_, c)| c.len() * 10 <= raw.len() * 9);
    let (req, wire) = match packed {
        Some((true, c)) => {
            let n = c.len() as u64;
            (Request::MemWriteZstd { memory, offset, raw_len, data: c }, n)
        }
        Some((false, c)) => {
            let n = c.len() as u64;
            (Request::MemWriteLz4 { memory, offset, raw_len, data: c }, n)
        }
        None => {
            let n = raw.len() as u64;
            (Request::MemWrite { memory, offset, data: raw }, n)
        }
    };
    WIRE_TOTAL.fetch_add(wire, std::sync::atomic::Ordering::Relaxed);
    *WIRE_BY_MEM.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).entry(memory).or_insert(0) += wire;
    let frame = Frame::new(0, flags::CRC, seq, 0, req.encode()).map_err(ClientError::Frame)?;
    Ok(frame.encode())
}

/// Czy zapisy pamięci używają prefiksu z lustra "ostatnio przesłane" (domyślnie tak; `GPU_ICD_NO_PFX=1` wyłącza, a `GPU_ICD_RUNS=1` też, bo tryb
/// RUNS pisze do pamięci bez aktualizacji lustra serwera).
fn mem_pfx() -> bool {
    static REST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    mem_zstd() && *REST.get_or_init(|| mem_dict().is_none() && std::env::var_os("GPU_ICD_NO_PFX").is_none() && std::env::var_os("GPU_ICD_RUNS").is_none())
}

/// Zapis pamięci jako ramka `MemWriteZstdPfx` (zstd z prefiksem = poprzednia treść zakresu), a gdy to nie daje 10% oszczędności, jako surowy
/// `MemWrite` (serwer i tak kopiuje go do swojego lustra). Obie strony zawsze mają to samo lustro, bo powstaje z tego samego ciągu zapisów.
fn mem_frame_pfx(seq: u16, memory: u64, offset: u64, raw: Vec<u8>, prefix: Vec<u8>) -> Result<Vec<u8>, ClientError> {
    let raw_len = raw.len() as u32;
    let packed = (raw.len() >= 4096)
        .then(|| {
            let mut cctx = zstd::zstd_safe::CCtx::create();
            let mut out: Vec<u8> = Vec::with_capacity(zstd::zstd_safe::compress_bound(raw.len()));
            let ok = cctx.set_parameter(zstd::zstd_safe::CParameter::CompressionLevel(zstd_level())).is_ok() && cctx.ref_prefix(&prefix).is_ok();
            (ok && cctx.compress2(&mut out, &raw).is_ok()).then_some(out)
        })
        .flatten()
        .filter(|c| c.len() * 10 <= raw.len() * 9);
    let (req, wire) = match packed {
        Some(c) => {
            let n = c.len() as u64;
            (Request::MemWriteZstdPfx { memory, offset, raw_len, raw_crc: gpu_proto::frame::crc32c(&raw), data: c }, n)
        }
        None => {
            let n = raw.len() as u64;
            (Request::MemWrite { memory, offset, data: raw }, n)
        }
    };
    WIRE_TOTAL.fetch_add(wire, std::sync::atomic::Ordering::Relaxed);
    *WIRE_BY_MEM.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).entry(memory).or_insert(0) += wire;
    let frame = Frame::new(0, flags::CRC, seq, 0, req.encode()).map_err(ClientError::Frame)?;
    Ok(frame.encode())
}

/// Kilka fragmentów jednej pamięci jako jedna ramka `MemWriteRunsPfx` (zstd z prefiksem = sklejone poprzednie treści; surowo, gdy to nie oszczędza 10%).
fn runs_pfx_frame(seq: u16, memory: u64, runs: Vec<(u64, u32)>, raw: Vec<u8>, prefix: Vec<u8>) -> Result<Vec<u8>, ClientError> {
    let raw_len = raw.len() as u32;
    let packed = (raw.len() >= 4096)
        .then(|| {
            let mut cctx = zstd::zstd_safe::CCtx::create();
            let mut out: Vec<u8> = Vec::with_capacity(zstd::zstd_safe::compress_bound(raw.len()));
            let ok = cctx.set_parameter(zstd::zstd_safe::CParameter::CompressionLevel(zstd_level())).is_ok() && cctx.ref_prefix(&prefix).is_ok();
            (ok && cctx.compress2(&mut out, &raw).is_ok()).then_some(out)
        })
        .flatten()
        .filter(|c| c.len() * 10 <= raw.len() * 9);
    let raw_crc = gpu_proto::frame::crc32c(&raw);
    let (is_packed, data) = match packed {
        Some(c) => (true, c),
        None => (false, raw),
    };
    let wire = data.len() as u64 + runs.len() as u64 * 12;
    WIRE_TOTAL.fetch_add(wire, std::sync::atomic::Ordering::Relaxed);
    *WIRE_BY_MEM.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).entry(memory).or_insert(0) += wire;
    let req = Request::MemWriteRunsPfx { memory, runs, raw_len, raw_crc, packed: is_packed, data };
    let frame = Frame::new(0, flags::CRC, seq, 0, req.encode()).map_err(ClientError::Frame)?;
    Ok(frame.encode())
}

/// Zapis fragmentów pamięci jako ramka `MemWriteRuns` (zawsze blok LZ4 sklejonych bajtów).
fn runs_frame(seq: u16, memory: u64, runs: Vec<(u64, u32)>, pages: Vec<(u32, u64)>, raw: Vec<u8>) -> Result<Vec<u8>, ClientError> {
    let data = gpu_proto::video::compress_lz4(&raw);
    let wire = data.len() as u64 + runs.len() as u64 * 12 + pages.len() as u64 * 12;
    WIRE_TOTAL.fetch_add(wire, std::sync::atomic::Ordering::Relaxed);
    *WIRE_BY_MEM.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).entry(memory).or_insert(0) += wire;
    let req = Request::MemWriteRuns { memory, runs, pages, raw_len: raw.len() as u32, data };
    let frame = Frame::new(0, flags::CRC, seq, 0, req.encode()).map_err(ClientError::Frame)?;
    Ok(frame.encode())
}

struct OutState {
    queue: VecDeque<Item>,
    bytes: usize,
    closed: bool,
    broken: bool,
}

/// Górny limit zakolejkowanych bajtów (potem producent czeka: ochrona pamięci i przeciwdziałanie bufferbloat).
const MAX_QUEUED_BYTES: usize = 64 << 20;

/// Połączenie sterujące: zapisy przez wątek nadawczy, odpowiedzi przez `Shared` (bez blokady instancji).
///
/// Wątki gry kodują ramki pod blokadą instancji (kolejność w strumieniu), ale same zapisy do gniazda (megabajty zapisów pamięci na klatkę)
/// robi osobny wątek: wcześniej `write_all` blokował wątek gry na czas wysłania, czyli ok. 60 ms na klatkę przy 7 MB zapisów.
pub struct Link {
    out: Arc<Out>,
    shared: Arc<Shared>,
    sender: Option<std::thread::JoinHandle<()>>,
    workers: Vec<std::thread::JoinHandle<()>>,
    seq: u16,
    direct: Option<Mutex<TcpStream>>,
    /// Ile żądań z odpowiedzią wysłano (numer biletu następnego takiego żądania). Zmieniany tylko pod blokadą instancji.
    sent: u64,
    /// Lustro "ostatnio przesłane w to miejsce" po pamięci (słownik zstd dla `MemWriteZstdPfx`); rośnie do największego zapisanego końca, reszta to zera.
    mirror: Arc<MirrorGate>,
    /// Numer tury następnego zapisu z prefiksem (nadawany pod blokadą instancji) i pamięci zwolnione od ostatniej tury.
    mturn: u64,
    forget_pending: Vec<u64>,
    /// Numer (16 bitów) ostatniej wysłanej ramki `Sync` i ostatniej ramki zapisu dla każdej pamięci: zależność odczytu na kanale odczytów (`read_dep`).
    last_sync: u16,
    mem_last: std::collections::HashMap<u64, u16>,
    /// Licznik wszystkich ramek strumienia sterującego bez zawijania i jego wartości przy `last_sync` i zapisach pamięci (`mem_last_n`): pozwalają ocenić WIEK zależności.
    n_frames: u64,
    last_sync_n: u64,
    mem_last_n: std::collections::HashMap<u64, u64>,
}

impl Link {
    /// Łączy się i wykonuje `Hello`; potem przejmuje gniazdo.
    #[allow(dead_code)]
    pub fn connect<A: ToSocketAddrs>(addr: A, token: &[u8]) -> Result<Link, ClientError> {
        Link::from_client(Client::connect(addr, token)?)
    }

    /// Przejmuje połączenie po uwierzytelnieniu (strumień jest wtedy "czysty": żadnych niewysłanych ani nieodebranych bajtów).
    pub fn from_client(client: Client) -> Result<Link, ClientError> {
        let (stream, seq) = client.into_stream();
        // Limit 30 s z uzgadniania nie może zostać na łączu roboczym: odpowiedź na długie oczekiwanie (fence, semafor, zaległa praca serwera) trwająca
        // dłużej kończyła się błędem odczytu i zerwaniem połączenia, a gra wisiała. Martwy serwer wykrywa keepalive TCP.
        stream.set_read_timeout(None)?;
        gpu_renderd::client::enable_keepalive(&stream);
        let reader = stream.try_clone()?;
        let direct_stream = stream.try_clone()?;
        let mut writer = stream;
        let shared = Arc::new(Shared { turn: Mutex::new(Turn { served: 0, broken: false, deferred: Vec::new(), draining: false }), cv: Condvar::new(), reader: Mutex::new(Reader { stream: reader, frames: FrameReader::new() }) });
        let out = Arc::new(Out { q: Mutex::new(OutState { queue: VecDeque::new(), bytes: 0, closed: false, broken: false }), data: Condvar::new(), space: Condvar::new(), work: Mutex::new(VecDeque::new()), work_cv: Condvar::new(), work_closed: std::sync::atomic::AtomicBool::new(false) });
        let (o, sh) = (out.clone(), shared.clone());
        let sender = std::thread::Builder::new()
            .name("gpu-icd-send".into())
            .spawn(move || loop {
                let buf = {
                    let mut g = o.q.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if let Some(b) = g.queue.pop_front() {
                            g.bytes -= b.queued_len();
                            LINK_BACKLOG.store(g.bytes, std::sync::atomic::Ordering::Relaxed);
                            o.space.notify_all();
                            break Some(b);
                        }
                        if g.closed {
                            break None;
                        }
                        g = o.data.wait(g).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let Some(item) = buf else { return };
                let mut stamp = None;
                let buf = match item {
                    Item::Bytes(b) => b,
                    Item::Stamped(b, ticket, t0) => {
                        stamp = Some((ticket, t0));
                        b
                    }
                    Item::Job(j) => match j.wait() {
                        Ok(b) => b,
                        Err(_) => continue,
                    },
                };
                if writer.write_all(&buf).is_err() {
                    // Zerwane połączenie: producenci i oczekujący na odpowiedzi dostają błąd zamiast czekać w nieskończoność.
                    o.q.lock().unwrap_or_else(|e| e.into_inner()).broken = true;
                    o.space.notify_all();
                    let mut t = sh.turn.lock().unwrap_or_else(|e| e.into_inner());
                    t.broken = true;
                    sh.cv.notify_all();
                    return;
                }
                if let Some((ticket, t0)) = stamp {
                    marks_written(ticket, t0);
                }
            })
            .map_err(ClientError::Io)?;
        let nworkers = std::env::var("GPU_ICD_FRAMERS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(2).min(8);
        let mut workers = Vec::new();
        for i in 0..nworkers {
            let o = out.clone();
            let h = std::thread::Builder::new()
                .name(format!("gpu-icd-frame{i}"))
                .spawn(move || loop {
                    let job = {
                        let mut g = o.work.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(j) = g.pop_front() {
                                break Some(j);
                            }
                            if o.work_closed.load(std::sync::atomic::Ordering::SeqCst) {
                                break None;
                            }
                            g = o.work_cv.wait(g).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    match job {
                        Some(j) => j.run(),
                        None => return,
                    }
                })
                .map_err(ClientError::Io)?;
            workers.push(h);
        }
        let direct = if std::env::var_os("GPU_ICD_SYNC_SEND").is_some() { Some(Mutex::new(direct_stream)) } else { None };
        Ok(Link { out, shared, sender: Some(sender), workers, seq, direct, sent: 0, mirror: Arc::new(MirrorGate { st: Default::default(), cv: Condvar::new(), dead: Default::default() }), mturn: 0, forget_pending: Vec::new(), last_sync: seq, mem_last: Default::default(), n_frames: 0, last_sync_n: 0, mem_last_n: Default::default() })
    }

    /// Opis stanu kolejki odpowiedzi (do diagnostyki zawieszeń).
    pub fn describe(&self) -> String {
        let t = self.shared.turn.lock().unwrap_or_else(|e| e.into_inner());
        let q = self.out.q.lock().unwrap_or_else(|e| e.into_inner());
        format!("odpowiedzi: wysłano zapytań {}, odebrano {}{}; kolejka nadawcza {} B w {} partiach{}", self.sent, t.served, if t.broken { " (POŁĄCZENIE ZERWANE)" } else { "" }, q.bytes, q.queue.len(), if q.broken { " (nadawca ZERWANY)" } else { "" })
    }

    /// (bajty, liczba partii) w kolejce nadawczej: ile danych stoi przed żądaniem, które właśnie wysyłamy.
    pub fn queued(&self) -> (usize, usize) {
        let q = self.out.q.lock().unwrap_or_else(|e| e.into_inner());
        (q.bytes as usize, q.queue.len())
    }

    /// Ramka strumienia sterującego, po której serwer może czytać `memory` na kanale odczytów: późniejsza z ostatniego `Sync` (GPU skończyło pracę
    /// zgłoszoną przed nim) i ostatniego zapisu tej pamięci przez aplikację (jej zapisy nie mogą zniknąć z odczytu).
    pub fn read_dep(&self, memory: u64) -> u16 {
        // Wiek liczymy licznikiem bez zawijania: zapis sprzed 33-65 tys. ramek wyglądał po zawinięciu na zależność "z przyszłości" i serwer czekał na niego do limitu.
        let (w, wn) = match self.mem_last.get(&memory) {
            Some(&w) => (w, self.mem_last_n.get(&memory).copied().unwrap_or(0)),
            None => (self.last_sync, self.last_sync_n),
        };
        let (dep, n) = if wn >= self.last_sync_n { (w, wn) } else { (self.last_sync, self.last_sync_n) };
        if self.n_frames.saturating_sub(n) > Self::READ_DEP_MAX_AGE {
            // Tak stara zależność jest na pewno wykonana (kolejka w locie jest o rzędy wielkości krótsza): podajemy najstarszą wartość, którą serwer uzna za przeszłą.
            return self.seq.wrapping_sub(Self::READ_DEP_MAX_AGE as u16);
        }
        dep
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// Najstarsza zależność odczytu (w ramkach) przekazywana serwerowi; starsze są pewnie wykonane (patrz `read_dep` i `readview::reached`).
const READ_DEP_MAX_AGE: u64 = 40000;

    /// Bilet, który dostanie następne żądanie z odpowiedzią.
    #[allow(dead_code)]
    pub fn next_ticket(&self) -> u64 {
        self.sent
    }

    fn frame(&mut self, req: &Request, out: &mut Vec<u8>) -> Result<(), ClientError> {
        self.seq = self.seq.wrapping_add(1);
        self.n_frames += 1;
        if matches!(req, Request::Sync) {
            self.last_sync = self.seq;
            self.last_sync_n = self.n_frames;
        }
        let frame = Frame::new(0, flags::CRC, self.seq, 0, req.encode()).map_err(ClientError::Frame)?;
        out.extend_from_slice(&frame.encode());
        Ok(())
    }

    fn enqueue(&self, bytes: Vec<u8>) -> Result<(), ClientError> {
        self.enqueue_item(Item::Bytes(bytes))
    }

    /// Zapis pamięci: numer ramki nadajemy tu (pod blokadą instancji, więc kolejność w strumieniu jest zachowana), a kompresję, CRC i kodowanie
    /// robi wątek nadawczy. Wołający oddaje własną kopię bajtów.
    pub fn send_mem(&mut self, memory: u64, offset: u64, raw: Vec<u8>) -> Result<(), ClientError> {
        self.seq = self.seq.wrapping_add(1);
        self.n_frames += 1;
        self.mem_last.insert(memory, self.seq);
        self.mem_last_n.insert(memory, self.n_frames);
        adapt_codec();
        let input = if mem_pfx() {
            // Prefiks i aktualizację lustra robi wątek roboczy w kolejności tur (numer nadajemy tu, pod blokadą instancji).
            JobInput::MemPfx(self.seq, memory, offset, raw, self.next_turn())
        } else {
            JobInput::Mem(self.seq, memory, offset, raw)
        };
        let len = match &input {
            JobInput::MemPfx(_, _, _, r, _) | JobInput::Mem(_, _, _, r) => r.len(),
            JobInput::Runs(_) | JobInput::RunsPfx(..) => 0,
        };
        let job = Arc::new(Job { len, input: Mutex::new(Some(input)), output: Mutex::new(None), done: Condvar::new() });
        if self.direct.is_none() && !self.workers.is_empty() {
            self.out.work.lock().unwrap_or_else(|e| e.into_inner()).push_back(job.clone());
            self.out.work_cv.notify_one();
        }
        self.enqueue_item(Item::Job(job))
    }

    /// Czy zapisy pamięci używają prefiksu (i mogą być łączone w ramki `MemWriteRunsPfx`).
    pub fn pfx_enabled() -> bool {
        mem_pfx()
    }

    /// Kilka fragmentów jednej pamięci w jednej ramce (`MemWriteRunsPfx`); `parts` to `(przesunięcie, bajty)`. Prefiks to kopia lustra z chwili wywołania
    /// (przed zapisem któregokolwiek fragmentu), potem lustro przejmuje nowe bajty; kolejność w strumieniu jak w `send_mem`.
    pub fn send_mem_group(&mut self, memory: u64, parts: &[(u64, &[u8])]) -> Result<(), ClientError> {
        self.seq = self.seq.wrapping_add(1);
        self.n_frames += 1;
        self.mem_last.insert(memory, self.seq);
        self.mem_last_n.insert(memory, self.n_frames);
        let total: usize = parts.iter().map(|p| p.1.len()).sum();
        let t0 = std::time::Instant::now();
        let mut raw = Vec::with_capacity(total);
        let mut runs = Vec::with_capacity(parts.len());
        for (o, d) in parts {
            raw.extend_from_slice(d);
            runs.push((*o, d.len() as u32));
        }
        // Prefiks i lustro: wątek roboczy w kolejności tur, z `raw` (kopii wysyłanej), a NIE z żywej pamięci aplikacji, która mogłaby się zmienić między odczytami.
        let turn = self.next_turn();
        GROUP_MIRROR_NS.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        let job = Arc::new(Job { len: total, input: Mutex::new(Some(JobInput::RunsPfx(self.seq, memory, runs, raw, turn))), output: Mutex::new(None), done: Condvar::new() });
        if self.direct.is_none() && !self.workers.is_empty() {
            self.out.work.lock().unwrap_or_else(|e| e.into_inner()).push_back(job.clone());
            self.out.work_cv.notify_one();
        }
        self.enqueue_item(Item::Job(job))
    }

    /// Zwolniona pamięć: lustro nie jest już potrzebne (serwer zdejmuje swoje przy zwolnieniu).
    pub fn forget_memory(&mut self, memory: u64) {
        // Lustro zdejmie najbliższe zadanie w swojej turze (uchwyty pamięci nie wracają, więc zwłoka nie szkodzi).
        self.forget_pending.push(memory);
    }

    /// Nadaje numer tury lustra następnemu zapisowi z prefiksem (pod blokadą instancji) i dołącza zaległe zwolnienia pamięci.
    fn next_turn(&mut self) -> MirrorTurn {
        let turn = self.mturn;
        self.mturn += 1;
        MirrorTurn { turn, forget: std::mem::take(&mut self.forget_pending), gate: self.mirror.clone() }
    }

    /// Zapis fragmentów jednej pamięci (patrz `Request::MemWriteRuns`); kolejność w strumieniu jak w `send_mem`, kompresja w wątku roboczym.
    pub fn send_mem_runs(&mut self, memory: u64, runs: Vec<(u64, u32)>, pages: Vec<(u32, u64)>, raw: Vec<u8>) -> Result<(), ClientError> {
        self.seq = self.seq.wrapping_add(1);
        self.n_frames += 1;
        self.mem_last.insert(memory, self.seq);
        self.mem_last_n.insert(memory, self.n_frames);
        let job = Arc::new(Job { len: raw.len(), input: Mutex::new(Some(JobInput::Runs((self.seq, memory, runs, pages, raw)))), output: Mutex::new(None), done: Condvar::new() });
        if self.direct.is_none() && !self.workers.is_empty() {
            self.out.work.lock().unwrap_or_else(|e| e.into_inner()).push_back(job.clone());
            self.out.work_cv.notify_one();
        }
        self.enqueue_item(Item::Job(job))
    }

    fn enqueue_item(&self, item: Item) -> Result<(), ClientError> {
        let len = item.queued_len();
        if let Some(w) = &self.direct {
            // `GPU_ICD_SYNC_SEND=1`: zapis w wątku wołającego (dawne zachowanie), do porównań przy szukaniu przyczyn zawieszeń.
            let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
            let bytes = match item {
                Item::Bytes(b) | Item::Stamped(b, ..) => b,
                Item::Job(j) => j.wait().map_err(|_| ClientError::Closed)?,
            };
            return w.write_all(&bytes).map_err(ClientError::Io);
        }
        let mut g = self.out.q.lock().unwrap_or_else(|e| e.into_inner());
        // Producent czeka tylko przy przepełnieniu (i pusta kolejka zawsze przyjmuje jedną, nawet większą partię).
        while g.bytes + len > MAX_QUEUED_BYTES && !g.queue.is_empty() && !g.broken {
            g = self.out.space.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        if g.broken {
            return Err(ClientError::Closed);
        }
        g.bytes += len;
        LINK_BACKLOG.store(g.bytes, std::sync::atomic::Ordering::Relaxed);
        g.queue.push_back(item);
        self.out.data.notify_one();
        Ok(())
    }

    pub fn send(&mut self, req: &Request) -> Result<(), ClientError> {
        let mut buf = Vec::new();
        self.frame(req, &mut buf)?;
        if expects_reply(req) {
            self.enqueue_item(Item::Stamped(buf, self.sent, std::time::Instant::now()))?;
            self.sent += 1;
        } else {
            self.enqueue(buf)?;
        }
        Ok(())
    }

    /// Wysyła kilka żądań jednym zapisem; zwraca bilet pierwszego z odpowiedzią (kolejne dostają następne numery).
    pub fn send_many(&mut self, reqs: &[Request]) -> Result<u64, ClientError> {
        let first = self.sent;
        let mut buf = Vec::new();
        for r in reqs {
            self.frame(r, &mut buf)?;
        }
        let replies = reqs.iter().filter(|r| expects_reply(r)).count() as u64;
        if replies > 0 {
            self.enqueue_item(Item::Stamped(buf, first, std::time::Instant::now()))?;
        } else {
            self.enqueue(buf)?;
        }
        self.sent += replies;
        Ok(first)
    }

    /// Wysyła żądanie z odpowiedzią i zwraca jego bilet; odpowiedź odbiera `Shared::recv_at(bilet)` (bez blokady instancji).
    pub fn send_t(&mut self, req: &Request) -> Result<u64, ClientError> {
        let ticket = self.sent;
        self.send(req)?;
        Ok(ticket)
    }

    /// Jak `call`, ale zwraca też bilet odpowiedzi (do wersjonowania stron: odpowiedź o niższym bilecie nie nadpisuje nowszej).
    pub fn call_t(&mut self, req: &Request) -> (u64, Result<Response, ClientError>) {
        let ticket = self.sent;
        (ticket, self.call(req))
    }

    /// Wysyła żądanie i czeka na odpowiedź (z blokadą instancji w ręku wołającego; kolejka biletów obowiązuje także tu).
    pub fn call(&mut self, req: &Request) -> Result<Response, ClientError> {
        let ticket = self.sent;
        self.send(req)?;
        if !expects_reply(req) {
            return Err(ClientError::Closed);
        }
        self.shared.recv_at(ticket)
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        // Wątek nadawczy dopisze to, co zostało w kolejce, i skończy; dopiero potem gniazdo się zamknie.
        self.out.q.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.out.data.notify_all();
        if let Some(h) = self.sender.take() {
            let _ = h.join();
        }
        self.mirror.dead.store(true, std::sync::atomic::Ordering::SeqCst);
        self.mirror.cv.notify_all();
        self.out.work_closed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.out.work_cv.notify_all();
        for h in self.workers.drain(..) {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpu_proto::frame::{flags, Frame};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serwer testowy: Hello -> HelloOk, potem na każde `MemRead { memory, .. }` odpowiada `Data([memory])` (po `delay_ms` * memory),
    /// `MemWrite` jest ciche.
    fn fake_server() -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut fr = FrameReader::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = match s.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                fr.push(&buf[..n]);
                while let Some(f) = fr.next_frame().unwrap() {
                    let resp = match Request::decode(&f.payload).unwrap() {
                        Request::Hello { .. } => Some(Response::HelloOk { version: gpu_proto::msg::PROTOCOL_VERSION }),
                        Request::MemRead { memory, .. } => Some(Response::Data(vec![memory as u8])),
                        _ => None,
                    };
                    if let Some(r) = resp {
                        let fr = Frame::new(0, flags::CRC, f.header.seq, 0, r.encode()).unwrap();
                        s.write_all(&fr.encode()).unwrap();
                    }
                }
            }
        });
        (addr, h)
    }

    #[test]
    fn responses_go_to_the_ticket_holders_even_when_they_ask_in_reverse_order() {
        let (addr, h) = fake_server();
        let mut link = Link::connect(addr, b"t").unwrap();
        // Trzy żądania z odpowiedzią i jedno ciche między nimi: bilety 0, 1, 2 (ciche nie dostaje biletu).
        let t0 = link.send_many(&[Request::MemRead { memory: 10, offset: 0, len: 1 }, Request::MemWrite { memory: 1, offset: 0, data: vec![1] }, Request::MemRead { memory: 20, offset: 0, len: 1 }]).unwrap();
        assert_eq!(t0, 0);
        link.send(&Request::MemRead { memory: 30, offset: 0, len: 1 }).unwrap();
        assert_eq!(link.next_ticket(), 3);
        let shared = link.shared();
        // Wątki pytają w odwrotnej kolejności: bilet 2 pierwszy, ale musi poczekać na 0 i 1.
        let (a, b, c) = (shared.clone(), shared.clone(), shared.clone());
        let t2 = std::thread::spawn(move || a.recv_at(2));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let t1 = std::thread::spawn(move || b.recv_at(1));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let t0h = std::thread::spawn(move || c.recv_at(0));
        assert_eq!(t0h.join().unwrap().unwrap(), Response::Data(vec![10]));
        assert_eq!(t1.join().unwrap().unwrap(), Response::Data(vec![20]));
        assert_eq!(t2.join().unwrap().unwrap(), Response::Data(vec![30]));
        // `call` wciąga się w tę samą kolejkę.
        assert_eq!(link.call(&Request::MemRead { memory: 40, offset: 0, len: 1 }).unwrap(), Response::Data(vec![40]));
        // Gniazdo ma dwa uchwyty (zapis i odczyt): serwer zobaczy koniec połączenia dopiero po zwolnieniu obu.
        drop(link);
        drop(shared);
        h.join().unwrap();
    }

    #[test]
    fn deferred_replies_are_drained_by_the_next_waiter_in_order() {
        let (addr, srv) = fake_server();
        let mut link = Link::connect(addr, b"t").unwrap();
        let before = DEFERRED_DONE.load(std::sync::atomic::Ordering::Relaxed);
        // Dwa żądania z odpowiedzią (zapytanie + Sync), na które nikt nie czeka, potem zwykłe wywołanie.
        let t = link.send_many(&[Request::MemRead { memory: 1, offset: 0, len: 1 }, Request::MemRead { memory: 2, offset: 0, len: 1 }]).unwrap();
        link.shared().defer(t);
        let r = link.call(&Request::MemRead { memory: 3, offset: 0, len: 1 }).unwrap();
        assert_eq!(r, Response::Data(vec![3]), "własna odpowiedź wraca poprawnie po odroczonych");
        let done = DEFERRED_DONE.load(std::sync::atomic::Ordering::Relaxed) - before;
        assert!(done >= 2, "odroczone odpowiedzi odebrane przez następnego czekającego: {done}");
        // Kolejne zwykłe wywołanie nadal działa (kolejność biletów nie rozjechała się).
        assert_eq!(link.call(&Request::MemRead { memory: 4, offset: 0, len: 1 }).unwrap(), Response::Data(vec![4]));
        drop(link);
        srv.join().unwrap();
    }

    #[test]
    fn a_broken_connection_wakes_every_waiter_with_an_error() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut fr = FrameReader::new();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            fr.push(&buf[..n]);
            let f = fr.next_frame().unwrap().unwrap();
            let ok = Frame::new(0, flags::CRC, f.header.seq, 0, Response::HelloOk { version: gpu_proto::msg::PROTOCOL_VERSION }.encode()).unwrap();
            s.write_all(&ok.encode()).unwrap();
            // zamyka połączenie bez odpowiedzi na kolejne żądania
        });
        let mut link = Link::connect(addr, b"t").unwrap();
        let t0 = link.send_many(&[Request::MemRead { memory: 1, offset: 0, len: 1 }, Request::MemRead { memory: 2, offset: 0, len: 1 }]).unwrap();
        srv.join().unwrap();
        let shared = link.shared();
        let s2 = shared.clone();
        let w = std::thread::spawn(move || s2.recv_at(t0 + 1));
        assert!(shared.recv_at(t0).is_err());
        assert!(w.join().unwrap().is_err(), "drugi oczekujący nie zawisa po zerwaniu połączenia");
    }
    /// Zapisy pamięci kompresowane równolegle przez wątki robocze docierają do serwera w kolejności wysłania i z tą samą zawartością.
    #[test]
    fn parallel_framing_keeps_memory_writes_in_order_and_intact() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        type Seen = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let srv = std::thread::spawn(move || {
            // Lustro serwera (jak w gpu-renderd): zera, każdy zapis kopiuje tu swoje bajty.
            let mut mirror: Vec<u8> = vec![0u8; 4 << 20];
            let (mut s, _) = l.accept().unwrap();
            let mut fr = FrameReader::new();
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = match s.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                fr.push(&buf[..n]);
                while let Some(f) = fr.next_frame().unwrap() {
                    match Request::decode(&f.payload).unwrap() {
                        Request::Hello { .. } => {
                            let r = Frame::new(0, flags::CRC, f.header.seq, 0, Response::HelloOk { version: gpu_proto::msg::PROTOCOL_VERSION }.encode()).unwrap();
                            s.write_all(&r.encode()).unwrap();
                        }
                        Request::MemWrite { offset, data, .. } => {
                            mirror[offset as usize..offset as usize + data.len()].copy_from_slice(&data);
                            seen2.lock().unwrap().push((offset, data));
                        }
                        Request::MemWriteLz4 { offset, raw_len, data, .. } => {
                            let raw = gpu_proto::video::decompress_lz4(&data, raw_len as usize).unwrap();
                            mirror[offset as usize..offset as usize + raw.len()].copy_from_slice(&raw);
                            seen2.lock().unwrap().push((offset, raw));
                        }
                        Request::MemWriteZstd { offset, raw_len, data, .. } => {
                            let raw = zstd::bulk::decompress(&data, raw_len as usize).unwrap();
                            mirror[offset as usize..offset as usize + raw.len()].copy_from_slice(&raw);
                            seen2.lock().unwrap().push((offset, raw));
                        }
                        Request::MemWriteRunsPfx { runs, raw_len, raw_crc, packed, data, .. } => {
                            let raw: Vec<u8> = if packed {
                                let mut prefix = Vec::new();
                                for &(o, l) in &runs {
                                    prefix.extend_from_slice(&mirror[o as usize..o as usize + l as usize]);
                                }
                                let mut raw: Vec<u8> = Vec::with_capacity(raw_len as usize);
                                let mut d = zstd::zstd_safe::DCtx::create();
                                d.ref_prefix(&prefix).unwrap();
                                d.decompress(&mut raw, &data).unwrap();
                                raw
                            } else {
                                data
                            };
                            assert_eq!(raw.len(), raw_len as usize);
                            assert_eq!(gpu_proto::frame::crc32c(&raw), raw_crc);
                            let mut at = 0usize;
                            for &(o, l) in &runs {
                                let part = raw[at..at + l as usize].to_vec();
                                mirror[o as usize..o as usize + l as usize].copy_from_slice(&part);
                                seen2.lock().unwrap().push((o, part));
                                at += l as usize;
                            }
                        }
                        Request::MemWriteZstdPfx { offset, raw_len, raw_crc, data, .. } => {
                            let (o, e) = (offset as usize, offset as usize + raw_len as usize);
                            let mut raw: Vec<u8> = Vec::with_capacity(raw_len as usize);
                            {
                                let mut d = zstd::zstd_safe::DCtx::create();
                                d.ref_prefix(&mirror[o..e]).unwrap();
                                d.decompress(&mut raw, &data).unwrap();
                            }
                            assert_eq!(raw.len(), raw_len as usize);
                            assert_eq!(gpu_proto::frame::crc32c(&raw), raw_crc);
                            mirror[o..e].copy_from_slice(&raw);
                            seen2.lock().unwrap().push((offset, raw));
                        }
                        Request::MemRead { .. } => {
                            let r = Frame::new(0, flags::CRC, f.header.seq, 0, Response::Data(vec![1]).encode()).unwrap();
                            s.write_all(&r.encode()).unwrap();
                        }
                        _ => {}
                    }
                }
            }
        });
        let mut link = Link::connect(addr, b"t").unwrap();
        let mut sent = Vec::new();
        let mut x = 0x1234_5678u32;
        for i in 0..200u64 {
            // Na przemian dobrze kompresowalne (zera), słabo (pseudolosowe) i małe (poniżej progu LZ4).
            let len = [64 * 1024, 8 * 1024, 100, 256 * 1024][(i % 4) as usize];
            let data: Vec<u8> = (0..len)
                .map(|k| match i % 3 {
                    0 => 0,
                    1 => {
                        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                        (x >> 24) as u8
                    }
                    _ => (k % 7) as u8,
                })
                .collect();
            if i % 5 == 4 {
                // Kilka fragmentów w jednej ramce (jedno z nich dobrze kompresowalne, jedno poniżej progu kompresji).
                let base = i * 4096;
                let parts: Vec<(u64, Vec<u8>)> = vec![(base, data.iter().take(5000).copied().collect()), (base + 1_000_000, data.iter().rev().take(12_000).copied().collect()), (base + 2_000_000, vec![(i % 251) as u8; 300])];
                let refs: Vec<(u64, &[u8])> = parts.iter().map(|(o, d)| (*o, d.as_slice())).collect();
                link.send_mem_group(7, &refs).unwrap();
                sent.extend(parts);
                continue;
            }
            link.send_mem(7, i * 4096, data.clone()).unwrap();
            sent.push((i * 4096, data));
        }
        // Odczyt po zapisach: odpowiedź przychodzi dopiero po wszystkich wcześniejszych ramkach (jedno połączenie, kolejność strumienia).
        assert_eq!(link.call(&Request::MemRead { memory: 7, offset: 0, len: 1 }).unwrap(), Response::Data(vec![1]));
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), sent.len());
        for (g, e) in got.iter().zip(&sent) {
            assert_eq!(g.0, e.0, "kolejność zapisów");
            assert_eq!(g.1, e.1, "zawartość zapisu pod {}", e.0);
        }
        drop(link);
        srv.join().unwrap();
    }

    /// Odpowiedź wolniejsza niż limit czasu z uzgadniania (30 s w produkcji) nie zrywa łącza roboczego.
    #[test]
    fn a_slow_reply_does_not_break_the_working_link() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut fr = FrameReader::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = match s.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                fr.push(&buf[..n]);
                while let Some(f) = fr.next_frame().unwrap() {
                    let resp = match Request::decode(&f.payload).unwrap() {
                        Request::Hello { .. } => Response::HelloOk { version: gpu_proto::msg::PROTOCOL_VERSION },
                        Request::MemRead { .. } => {
                            std::thread::sleep(std::time::Duration::from_millis(700));
                            Response::Data(vec![5])
                        }
                        _ => continue,
                    };
                    s.write_all(&Frame::new(0, flags::CRC, f.header.seq, 0, resp.encode()).unwrap().encode()).unwrap();
                }
            }
        });
        let mut client = Client::connect(addr, b"t").unwrap();
        client.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
        let mut link = Link::from_client(client).unwrap();
        assert_eq!(link.call(&Request::MemRead { memory: 1, offset: 0, len: 1 }).unwrap(), Response::Data(vec![5]), "odpowiedź po 700 ms mimo limitu 200 ms z uzgadniania");
        drop(link);
        srv.join().unwrap();
    }

}

#[cfg(test)]
mod route_tests {
    use super::route_iface;

    const ROUTE: &str = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT
wlan0\t00000000\t0100A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
enp3s0\t00002A0A\t00000000\t0001\t0\t0\t100\t0000FFFF\t0\t0\t0
wlan0\t0000A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0
";

    #[test]
    fn longest_prefix_route_picks_the_direct_link_interface() {
        assert_eq!(route_iface(ROUTE, "10.42.0.2".parse().unwrap()).as_deref(), Some("enp3s0"));
        assert_eq!(route_iface(ROUTE, "192.168.0.83".parse().unwrap()).as_deref(), Some("wlan0"));
        assert_eq!(route_iface(ROUTE, "8.8.8.8".parse().unwrap()).as_deref(), Some("wlan0"), "trasa domyślna");
    }
}

#[cfg(test)]
mod adapt_tests {
    use super::Adapt;

    #[test]
    fn switches_only_after_enough_consecutive_active_low_windows() {
        let mut a = Adapt::default();
        let mut wire = 0u64;
        assert!(!a.observe(0.0, wire));
        // 30 MB/s przez 6 okien po 5 s => przejście dokładnie przy szóstym oknie.
        for k in 1..=6 {
            wire += 150_000_000;
            let sw = a.observe(5.0 * k as f64, wire);
            assert_eq!(sw, k == 6, "okno {k}");
        }
    }

    #[test]
    fn heavy_or_idle_windows_reset_the_count() {
        let mut a = Adapt::default();
        let mut wire = 0u64;
        a.observe(0.0, wire);
        for k in 1..=5 {
            wire += 150_000_000; // 30 MB/s
            assert!(!a.observe(5.0 * k as f64, wire));
        }
        wire += 600_000_000; // 120 MB/s: zerowanie
        assert!(!a.observe(30.0, wire));
        for k in 1..=5 {
            wire += 150_000_000;
            assert!(!a.observe(30.0 + 5.0 * k as f64, wire), "po zerowaniu trzeba znowu 6 okien");
        }
        // okno bez ruchu (menu): też zeruje, nie przybliża przejścia
        assert!(!a.observe(60.0, wire));
        wire += 150_000_000;
        assert!(!a.observe(65.0, wire));
    }
}
