//! Prosty klient synchroniczny protokołu M1 (do narzędzi diagnostycznych i testów).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use gpu_proto::frame::{flags, Frame, FrameReader};
use gpu_proto::msg::{MsgError, Request, Response, PROTOCOL_VERSION};

#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    Closed,
    Frame(gpu_proto::frame::FrameError),
    Msg(MsgError),
    /// Serwer odpowiedział błędem protokołu lub `VkResult` (kod, opis).
    Remote(i32, String),
    Unexpected(Response),
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::Io(e)
    }
}

pub struct Client {
    stream: TcpStream,
    reader: FrameReader,
    seq: u16,
}

impl Client {
    /// Łączy się i wykonuje `Hello` z tokenem.
    pub fn connect<A: ToSocketAddrs>(addr: A, token: &[u8]) -> Result<Client, ClientError> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut c = Client { stream, reader: FrameReader::new(), seq: 0 };
        match c.call(&Request::Hello { version: PROTOCOL_VERSION, token: token.to_vec() })? {
            Response::HelloOk { .. } => Ok(c),
            other => Err(ClientError::Unexpected(other)),
        }
    }

    /// Wysyła żądanie bez czekania na odpowiedź (potokowanie; kolejność w strumieniu jest zachowana).
    pub fn send(&mut self, req: &Request) -> Result<(), ClientError> {
        self.seq = self.seq.wrapping_add(1);
        let frame = Frame::new(0, flags::CRC, self.seq, 0, req.encode()).map_err(ClientError::Frame)?;
        self.stream.write_all(&frame.encode())?;
        Ok(())
    }

    /// Czeka na jedną odpowiedź; `Response::Error` zamienia na `Err(Remote)`.
    pub fn recv(&mut self) -> Result<Response, ClientError> {
        let mut buf = [0u8; 65536];
        loop {
            if let Some(f) = self.reader.next_frame().map_err(ClientError::Frame)? {
                return match Response::decode(&f.payload).map_err(ClientError::Msg)? {
                    Response::Error { code, message } => Err(ClientError::Remote(code, message)),
                    r => Ok(r),
                };
            }
            let n = self.stream.read(&mut buf)?;
            if n == 0 {
                return Err(ClientError::Closed);
            }
            self.reader.push(&buf[..n]);
        }
    }

    /// Wysyła żądanie i czeka na odpowiedź.
    pub fn call(&mut self, req: &Request) -> Result<Response, ClientError> {
        self.send(req)?;
        self.recv()
    }
}
