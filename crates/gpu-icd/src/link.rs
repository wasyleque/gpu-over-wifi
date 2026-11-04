//! Połączenie sterujące z serwerem z rozdzielonym zapisem i odczytem odpowiedzi.
//!
//! Serwer odpowiada na żądania w kolejności ich wysłania, a po stronie klienta zapisy idą pod blokadą instancji (kolejność w strumieniu).
//! Wcześniej wątek czekał na odpowiedź TRZYMAJĄC blokadę instancji, więc każde oczekiwanie (fence, semafor, zapytanie) zatrzymywało
//! wszystkie pozostałe wątki gry na czas obiegu po sieci (w Cyberpunku 250 s oczekiwania na blokadę przy 100 s jej trzymania).
//! Tu każde żądanie z odpowiedzią dostaje bilet (kolejny numer) w chwili wysłania, a odpowiedzi odbiera się w kolejności biletów bez
//! blokady instancji: wątek o bilecie `k` czeka, aż odczytano odpowiedzi `0..k`, czyta swoją i przekazuje turę dalej.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex};

use gpu_proto::frame::FrameReader;
use gpu_proto::msg::{Request, Response};
use gpu_renderd::client::{Client, ClientError};

/// Czy serwer odpowie na to żądanie (`Cmds`, `MemWrite*` i `PresentFrame` są ciche, o ile się powiodą).
pub fn expects_reply(req: &Request) -> bool {
    !matches!(req, Request::Cmds { .. } | Request::MemWrite { .. } | Request::MemWriteLz4 { .. } | Request::PresentFrame { .. })
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

/// Połączenie sterujące: zapisy przez `Client` (pod blokadą instancji), odpowiedzi przez `Shared` (bez niej).
pub struct Link {
    writer: Client,
    shared: Arc<Shared>,
    /// Ile żądań z odpowiedzią wysłano (numer biletu następnego takiego żądania). Zmieniany tylko pod blokadą instancji.
    sent: u64,
}

impl Link {
    /// Łączy się i wykonuje `Hello`; potem przejmuje kopię gniazda do odczytu.
    #[allow(dead_code)]
    pub fn connect<A: ToSocketAddrs>(addr: A, token: &[u8]) -> Result<Link, ClientError> {
        Link::from_client(Client::connect(addr, token)?)
    }

    /// Przejmuje połączenie po uwierzytelnieniu (strumień jest wtedy "czysty": żadnych niewysłanych ani nieodebranych bajtów).
    pub fn from_client(client: Client) -> Result<Link, ClientError> {
        let stream = client.shutdown_handle()?;
        Ok(Link { writer: client, shared: Arc::new(Shared { turn: Mutex::new(Turn { served: 0, broken: false }), cv: Condvar::new(), reader: Mutex::new(Reader { stream, frames: FrameReader::new() }) }), sent: 0 })
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// Bilet, który dostanie następne żądanie z odpowiedzią.
    #[allow(dead_code)]
    pub fn next_ticket(&self) -> u64 {
        self.sent
    }

    pub fn send(&mut self, req: &Request) -> Result<(), ClientError> {
        self.writer.send(req)?;
        if expects_reply(req) {
            self.sent += 1;
        }
        Ok(())
    }

    /// Wysyła kilka żądań jednym zapisem; zwraca bilet pierwszego z odpowiedzią (kolejne dostają następne numery).
    pub fn send_many(&mut self, reqs: &[Request]) -> Result<u64, ClientError> {
        let first = self.sent;
        self.writer.send_many(reqs)?;
        self.sent += reqs.iter().filter(|r| expects_reply(r)).count() as u64;
        Ok(first)
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
}
