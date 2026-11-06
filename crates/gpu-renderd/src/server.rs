//! Serwer TCP: jedno połączenie = jeden wątek, własny `VulkanBackend` i własna sesja.

use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::conn::{serve_connection, ServeError};
use crate::handler::Session;
use crate::vk::backend::VulkanBackend;

/// Bezczynność na gnieździe, po której zamykane jest połączenie, które się NIE uwierzytelniło (uwierzytelnione trwa do końca, patrz `conn.rs`).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Obsługuje jedno już zaakceptowane połączenie aż do jego końca.
pub fn handle_stream(mut stream: TcpStream, token: Vec<u8>) -> Result<(), ServeError> {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
    crate::client::enable_keepalive(&stream);
    let backend = VulkanBackend::new().map_err(|_| ServeError::Io(ErrorKind::Other))?;
    let mut session = Session::new(backend, token);
    let mut reader = stream.try_clone().map_err(|e| ServeError::Io(e.kind()))?;
    serve_connection(&mut session, &mut reader, &mut stream)
}

struct ConnGuard(Arc<AtomicUsize>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Pętla akceptująca; nie wraca (błąd `accept` jest logowany i pętla trwa).
/// Nadmiarowe połączenia ponad `max_conns` są od razu zamykane.
pub fn run(listener: TcpListener, token: Vec<u8>, max_conns: usize) -> ! {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(x) => x,
            Err(e) => {
                eprintln!("accept: {e}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= max_conns {
            active.fetch_sub(1, Ordering::SeqCst);
            eprintln!("{peer}: odrzucone (limit {max_conns} połączeń)");
            continue;
        }
        let guard = ConnGuard(active.clone());
        let token = token.clone();
        std::thread::spawn(move || {
            let _guard = guard;
            match handle_stream(stream, token) {
                Ok(()) => eprintln!("{peer}: zamknięte"),
                Err(e) => eprintln!("{peer}: zakończone błędem {e:?}"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpu_proto::frame::{flags, Frame, FrameReader};
    use gpu_proto::msg::{PdFunc, Request, Response, PROTOCOL_VERSION};
    use std::io::{Read, Write};

    struct Client {
        s: TcpStream,
        r: FrameReader,
        seq: u16,
    }

    impl Client {
        fn call(&mut self, req: &Request) -> Response {
            self.seq += 1;
            let f = Frame::new(0, flags::CRC, self.seq, 0, req.encode()).unwrap();
            self.s.write_all(&f.encode()).unwrap();
            let mut buf = [0u8; 4096];
            loop {
                if let Some(fr) = self.r.next_frame().unwrap() {
                    assert_eq!(fr.header.seq, self.seq);
                    return Response::decode(&fr.payload).unwrap();
                }
                let n = self.s.read(&mut buf).unwrap();
                assert!(n > 0, "serwer zamknął połączenie");
                self.r.push(&buf[..n]);
            }
        }
    }

    /// Serwer na losowym porcie: akceptuje jedno połączenie i obsługuje je w osobnym wątku.
    fn start(token: &'static [u8]) -> (Client, std::thread::JoinHandle<Result<(), ServeError>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            handle_stream(s, token.to_vec())
        });
        let s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        (Client { s, r: FrameReader::new(), seq: 0 }, h)
    }

    #[test]
    fn full_enumeration_over_real_tcp_on_real_vulkan() {
        if VulkanBackend::new().is_err() {
            eprintln!("POMINIĘTO: brak libvulkan");
            return;
        }
        let (mut c, h) = start(b"sekret");
        assert!(matches!(c.call(&Request::Hello { version: PROTOCOL_VERSION, token: b"sekret".to_vec() }), Response::HelloOk { .. }));
        let Response::U32(v) = c.call(&Request::EnumInstanceVersion) else { panic!() };
        assert!(v >= 0x0040_1000);
        let Response::Handle(i) = c.call(&Request::CreateInstance { api_version: 0x0040_1000, extensions: vec![] }) else { panic!() };
        let Response::Handles(p) = c.call(&Request::EnumPhysicalDevices { instance: i }) else { panic!() };
        let Response::Data(d) = c.call(&Request::GetPhysicalDevice { pdev: p[0], func: PdFunc::Properties2, arg: 0, count: 0, chain: vec![] }) else { panic!() };
        assert_eq!(d.len(), 840);
        drop(c);
        assert_eq!(h.join().unwrap(), Ok(()), "czyste zamknięcie przez klienta");
    }

    #[test]
    fn wrong_token_over_tcp_is_rejected_and_connection_closed() {
        let (mut c, h) = start(b"sekret");
        assert!(matches!(c.call(&Request::Hello { version: PROTOCOL_VERSION, token: b"zly".to_vec() }), Response::Error { .. }));
        assert_eq!(h.join().unwrap(), Err(ServeError::AuthFailed));
    }

    #[test]
    fn garbage_bytes_close_the_connection_without_crashing_the_server() {
        let (mut c, h) = start(b"sekret");
        c.s.write_all(&[0xFF; 64]).unwrap();
        assert!(h.join().unwrap().is_err());
    }
}
