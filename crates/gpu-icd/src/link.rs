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
    !matches!(req, Request::Cmds { .. } | Request::MemWrite { .. } | Request::MemWriteLz4 { .. } | Request::MemWriteZstd { .. } | Request::MemWriteRuns { .. } | Request::PresentFrame { .. })
}

struct Turn {
    /// Ile odpowiedzi już odczytano (numer biletu, który jest teraz na swojej kolei).
    served: u64,
    /// Odczyt zawiódł (rozłączenie, błąd ramki): wszyscy oczekujący dostają błąd zamiast czekać w nieskończoność.
    broken: bool,
}

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
    /// Odbiera odpowiedź dla biletu `ticket` (czeka na swoją kolej; nie wymaga blokady instancji).
    pub fn recv_at(&self, ticket: u64) -> Result<Response, ClientError> {
        {
            let mut t = self.turn.lock().unwrap_or_else(|e| e.into_inner());
            while t.served != ticket {
                if t.broken {
                    return Err(ClientError::Closed);
                }
                t = self.cv.wait(t).unwrap_or_else(|e| e.into_inner());
            }
            if t.broken {
                return Err(ClientError::Closed);
            }
        }
        // Tylko wątek, którego jest kolej, dochodzi tutaj, więc blokada odczytu nie jest rywalizowana.
        let r = self.read_one();
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
    Runs(RunsInput),
}
type JobOutput = Result<Vec<u8>, ()>;

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
    Job(Arc<Job>),
}

impl Item {
    fn queued_len(&self) -> usize {
        match self {
            Item::Bytes(b) => b.len(),
            Item::Job(j) => j.len,
        }
    }
}

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
fn mem_zstd() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("GPU_ICD_MEM_LZ4").is_none())
}

/// Zapis pamięci jako ramka: zstd (albo LZ4) tylko gdy oszczędza co najmniej 10% (małe i źle kompresowalne, np. tekstury BCn, idą surowo).
fn mem_frame(seq: u16, memory: u64, offset: u64, raw: Vec<u8>) -> Result<Vec<u8>, ClientError> {
    let raw_len = raw.len() as u32;
    let packed = (raw.len() >= 4096)
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
        let shared = Arc::new(Shared { turn: Mutex::new(Turn { served: 0, broken: false }), cv: Condvar::new(), reader: Mutex::new(Reader { stream: reader, frames: FrameReader::new() }) });
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
                let buf = match item {
                    Item::Bytes(b) => b,
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
        Ok(Link { out, shared, sender: Some(sender), workers, seq, direct, sent: 0 })
    }

    /// Opis stanu kolejki odpowiedzi (do diagnostyki zawieszeń).
    pub fn describe(&self) -> String {
        let t = self.shared.turn.lock().unwrap_or_else(|e| e.into_inner());
        let q = self.out.q.lock().unwrap_or_else(|e| e.into_inner());
        format!("odpowiedzi: wysłano zapytań {}, odebrano {}{}; kolejka nadawcza {} B w {} partiach{}", self.sent, t.served, if t.broken { " (POŁĄCZENIE ZERWANE)" } else { "" }, q.bytes, q.queue.len(), if q.broken { " (nadawca ZERWANY)" } else { "" })
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// Bilet, który dostanie następne żądanie z odpowiedzią.
    #[allow(dead_code)]
    pub fn next_ticket(&self) -> u64 {
        self.sent
    }

    fn frame(&mut self, req: &Request, out: &mut Vec<u8>) -> Result<(), ClientError> {
        self.seq = self.seq.wrapping_add(1);
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
        let job = Arc::new(Job { len: raw.len(), input: Mutex::new(Some(JobInput::Mem(self.seq, memory, offset, raw))), output: Mutex::new(None), done: Condvar::new() });
        if self.direct.is_none() && !self.workers.is_empty() {
            self.out.work.lock().unwrap_or_else(|e| e.into_inner()).push_back(job.clone());
            self.out.work_cv.notify_one();
        }
        self.enqueue_item(Item::Job(job))
    }

    /// Zapis fragmentów jednej pamięci (patrz `Request::MemWriteRuns`); kolejność w strumieniu jak w `send_mem`, kompresja w wątku roboczym.
    pub fn send_mem_runs(&mut self, memory: u64, runs: Vec<(u64, u32)>, pages: Vec<(u32, u64)>, raw: Vec<u8>) -> Result<(), ClientError> {
        self.seq = self.seq.wrapping_add(1);
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
                Item::Bytes(b) => b,
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
        g.queue.push_back(item);
        self.out.data.notify_one();
        Ok(())
    }

    pub fn send(&mut self, req: &Request) -> Result<(), ClientError> {
        let mut buf = Vec::new();
        self.frame(req, &mut buf)?;
        self.enqueue(buf)?;
        if expects_reply(req) {
            self.sent += 1;
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
        self.enqueue(buf)?;
        self.sent += reqs.iter().filter(|r| expects_reply(r)).count() as u64;
        Ok(first)
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
                        Request::MemWrite { offset, data, .. } => seen2.lock().unwrap().push((offset, data)),
                        Request::MemWriteLz4 { offset, raw_len, data, .. } => {
                            let raw = gpu_proto::video::decompress_lz4(&data, raw_len as usize).unwrap();
                            seen2.lock().unwrap().push((offset, raw));
                        }
                        Request::MemWriteZstd { offset, raw_len, data, .. } => {
                            let raw = zstd::bulk::decompress(&data, raw_len as usize).unwrap();
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
