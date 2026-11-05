//! Pętla jednego połączenia: ramki z czytnika -> `Request` -> `Session` -> ramki odpowiedzi do pisarza.
//! Niezależna od gniazd (`Read`/`Write`), więc testowalna w pamięci.

use std::io::{Read, Write};

use gpu_proto::frame::{flags, Frame, FrameError, FrameReader};
use gpu_proto::msg::{MsgError, Request, Response};

use crate::handler::DeltaItem;
use crate::handler::{GpuBackend, Session, ERR_PROTOCOL};

/// Jedyny strumień używany w M1: sterowanie (żądanie -> odpowiedź).
pub const STREAM_CONTROL: u8 = 0;
/// Strumień ramek obrazu serwer -> klient (połączenie wideo).
pub const STREAM_VIDEO: u8 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum ServeError {
    /// Błąd wejścia/wyjścia (także EOF w środku ramki: `UnexpectedEof`).
    Io(std::io::ErrorKind),
    Frame(FrameError),
    Msg(MsgError),
    /// Nieudane uwierzytelnienie: odpowiedź błędu została wysłana, połączenie zamknięte.
    AuthFailed,
    /// Ramka na nieznanym strumieniu albo ze skompresowanym payloadem (jeszcze nieobsługiwane).
    Unsupported,
}

impl From<FrameError> for ServeError {
    fn from(e: FrameError) -> Self {
        ServeError::Frame(e)
    }
}

impl From<std::io::Error> for ServeError {
    fn from(e: std::io::Error) -> Self {
        ServeError::Io(e.kind())
    }
}

fn send_reply<W: Write>(writer: &mut W, req_frame: &Frame, response: &Response) -> Result<(), ServeError> {
    let f = Frame::new(req_frame.header.stream, flags::CRC, req_frame.header.seq, req_frame.header.epoch, response.encode())?;
    writer.write_all(&f.encode())?;
    writer.flush()?;
    Ok(())
}

/// Pętla połączenia wideo: wysyła ramki sesji (strumień 1, bez CRC: TCP chroni dane, a suma kosztuje 1,9 MB na klatkę)
/// aż kanał się zamknie (sesja sterująca skończyła) albo zapis się nie powiedzie (klient odszedł).
fn video_loop<W: Write>(rx: std::sync::mpsc::Receiver<Vec<u8>>, writer: &mut W) -> Result<(), ServeError> {
    let mut seq = 0u16;
    while let Ok(payload) = rx.recv() {
        let f = Frame::new(STREAM_VIDEO, 0, seq, 0, payload)?;
        writer.write_all(&f.encode())?;
        writer.flush()?;
        seq = seq.wrapping_add(1);
    }
    Ok(())
}

/// Żądanie odczytu różnic pamięci jako (pamięć, zakresy albo `None` = cała pamięć); inne żądania dają `None`.
fn delta_item(req: &Request) -> Option<DeltaItem> {
    match req {
        Request::MemReadDelta { memory } => Some((*memory, None)),
        Request::MemReadDeltaRanges { memory, ranges, .. } => Some((*memory, Some(ranges.clone()))),
        _ => None,
    }
}

/// Wypisuje zaległe odpowiedzi jednym zapisem (kolejność bajtów w strumieniu bez zmian).
fn flush_pending<W: Write>(writer: &mut W, pending: &mut Vec<u8>) -> Result<(), ServeError> {
    if !pending.is_empty() {
        writer.write_all(pending)?;
        writer.flush()?;
        pending.clear();
    }
    Ok(())
}

/// Obsługuje połączenie aż do czystego EOF (na granicy ramki => `Ok`) albo błędu.
///
/// Zasady:
/// - odpowiedź niesie `stream`, `seq` i `epoch` żądania oraz flagę CRC;
/// - żądanie z `stream != STREAM_CONTROL` lub z flagą ZSTD => `Err(Unsupported)` (bez odpowiedzi);
/// - payload, którego nie da się zdekodować: wyślij `Response::Error{code: ERR_PROTOCOL, ..}`, potem `Err(Msg(..))`;
/// - jeśli sesja nie jest uwierzytelniona po obsłużeniu żądania: odpowiedź (błąd) zostaje wysłana i zwracamy `Err(AuthFailed)`;
/// - błąd ramki (np. zły CRC, zarezerwowane flagi) => `Err(Frame(..))` bez odpowiedzi;
/// - `session.cleanup()` jest wywoływane ZAWSZE na końcu (także przy błędach), dokładnie raz.
///
/// Czyta porcjami po najwyżej 64 KiB; nie może panikować na żadnych danych.
pub fn serve_connection<R: Read, W: Write, B: GpuBackend>(
    session: &mut Session<B>,
    reader: &mut R,
    writer: &mut W,
) -> Result<(), ServeError> {
    let mut fr = FrameReader::new();
    let mut buf = vec![0u8; 64 * 1024];
    // Odpowiedzi na wszystkie żądania z jednego odczytu idą jednym zapisem (patrz `flush_pending`): mniej pakietów i wywołań systemowych.
    let mut pending: Vec<u8> = Vec::new();
    // Ramki wyjęte z czytnika przy podglądaniu następnych żądań (patrz grupa odczytów różnic niżej).
    let mut ahead: std::collections::VecDeque<Frame> = std::collections::VecDeque::new();
    let mut group_left = 0usize;

    // Inner function to handle the main loop logic
    let result = (|| -> Result<(), ServeError> {
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // EOF - check if we have any buffered data
                    if fr.buffered() == 0 {
                        return Ok(());
                    } else {
                        return Err(ServeError::Io(std::io::ErrorKind::UnexpectedEof));
                    }
                }
                Ok(n) => {
                    // Read some data, push to frame reader
                    fr.push(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    // Interrupted, try again
                    continue;
                }
                Err(e) => {
                    return Err(ServeError::Io(e.kind()));
                }
            }

            // Process all complete frames from the buffer
            loop {
                let frame = match ahead.pop_front() {
                    Some(f) => f,
                    None => match fr.next_frame() {
                        Ok(Some(f)) => f,
                        Ok(None) => break,
                        Err(e) => {
                            flush_pending(writer, &mut pending)?;
                            return Err(e.into());
                        }
                    },
                };
                // Check if frame is on the correct stream or has ZSTD compression (unsupported)
                if frame.header.stream != STREAM_CONTROL || frame.header.flags & flags::ZSTD != 0 {
                    flush_pending(writer, &mut pending)?;
                    return Err(ServeError::Unsupported);
                }

                // Decode the request
                let req = match Request::decode(&frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        // Send error response and then return the decode error
                        let error_resp = Response::Error {
                            code: ERR_PROTOCOL,
                            message: "malformed request".to_string(),
                        };
                        let error_frame = Frame::new(
                            frame.header.stream,
                            flags::CRC,
                            frame.header.seq,
                            frame.header.epoch,
                            error_resp.encode(),
                        )?;
                        flush_pending(writer, &mut pending)?;
                        writer.write_all(&error_frame.encode())?;
                        writer.flush()?;
                        return Err(ServeError::Msg(e));
                    }
                };

                // Handle the request
                // Połączenie wideo: po uwierzytelnieniu `AttachVideo` zamienia to połączenie w strumień ramek sesji sterującej.
                if let (Request::AttachVideo { session: sid }, true) = (&req, session.is_authenticated()) {
                    flush_pending(writer, &mut pending)?;
                    let reply = match crate::videohub::claim(*sid) {
                        Some(rx) => {
                            send_reply(writer, &frame, &Response::U32(0))?;
                            return video_loop(rx, writer);
                        }
                        None => Response::Error { code: ERR_PROTOCOL, message: "unknown or already attached session".to_string() },
                    };
                    send_reply(writer, &frame, &reply)?;
                    continue;
                }
                // Kolejne żądania odczytu różnic pamięci, które już przyszły (klient odświeża po jednym oczekiwaniu kilka pamięci), idą do backendu jako
                // grupa: jedna kopia GPU i jeden płotek zamiast jednego na pamięć. Odpowiedzi nadal wychodzą po jednej, w kolejności żądań.
                if let Some(item) = delta_item(&req) {
                    if group_left == 0 && session.is_authenticated() {
                        let mut items = vec![item];
                        while let Ok(Some(f)) = fr.next_frame() {
                            let next = if f.header.stream == STREAM_CONTROL { Request::decode(&f.payload).ok().and_then(|r| delta_item(&r)) } else { None };
                            ahead.push_back(f);
                            match next {
                                Some(it) => items.push(it),
                                None => break,
                            }
                        }
                        if items.len() >= 2 {
                            session.prefetch_deltas(&items);
                            group_left = items.len();
                        }
                    }
                } else if group_left > 0 {
                    session.prefetch_end();
                    group_left = 0;
                }
                let handled_delta = delta_item(&req).is_some();
                let Some(response) = session.handle_request(req) else { continue };
                if handled_delta && group_left > 0 {
                    group_left -= 1;
                    if group_left == 0 {
                        session.prefetch_end();
                    }
                }

                // Encode and send the response
                let response_frame = Frame::new(
                    frame.header.stream,
                    flags::CRC,
                    frame.header.seq,
                    frame.header.epoch,
                    response.encode(),
                )?;
                pending.extend_from_slice(&response_frame.encode());

                // Check if authentication was successful
                if !session.is_authenticated() {
                    flush_pending(writer, &mut pending)?;
                    return Err(ServeError::AuthFailed);
                }
            }
            // Przetworzono wszystko, co przyszło w tym odczycie: odpowiedzi idą razem.
            flush_pending(writer, &mut pending)?;
        }
    })();

    // Always call cleanup on exit
    session.cleanup();

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::GpuBackend;
    use gpu_proto::msg::{PdFunc, PROTOCOL_VERSION};
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Mock {
        log: Rc<RefCell<Vec<String>>>,
    }

    impl GpuBackend for Mock {
        fn instance_version(&mut self) -> Result<u32, i32> {
            Ok(0x0040_3000)
        }
        fn instance_extensions(&mut self) -> Result<Vec<(String, u32)>, i32> {
            Ok(vec![])
        }
        fn create_instance(&mut self, _: u32, _: &[String]) -> Result<u64, i32> {
            self.log.borrow_mut().push("create".into());
            Ok(500)
        }
        fn destroy_instance(&mut self, i: u64) -> Result<(), i32> {
            self.log.borrow_mut().push(format!("destroy {i}"));
            Ok(())
        }
        fn enum_physical_devices(&mut self, _: u64) -> Result<Vec<u64>, i32> {
            Ok(vec![])
        }
        fn get_physical_device(
            &mut self,
            _: u64,
            _: PdFunc,
            _: u32,
            _: u32,
            _: &[u32],
        ) -> Result<Vec<u8>, i32> {
            Ok(vec![])
        }
        fn device_extensions(&mut self, _: u64) -> Result<Vec<(String, u32)>, i32> {
            Ok(vec![])
        }
        fn mem_read(&mut self, _: u64, _: u64, len: u32) -> Result<Vec<u8>, i32> {
            Ok(vec![9; len as usize])
        }
        fn mem_prefetch(&mut self, items: &[DeltaItem]) {
            self.log.borrow_mut().push(format!("prefetch {:?}", items.iter().map(|i| i.0).collect::<Vec<_>>()));
        }
        fn mem_prefetch_end(&mut self) {
            self.log.borrow_mut().push("prefetch_end".into());
        }
        fn mem_read_delta(&mut self, memory: u64, _: usize) -> Result<Vec<u8>, i32> {
            self.log.borrow_mut().push(format!("delta {memory}"));
            Ok(vec![memory as u8])
        }
        fn mem_read_delta_ranges(&mut self, memory: u64, _: &[(u64, u64)], _: usize, _: bool) -> Result<Vec<u8>, i32> {
            self.log.borrow_mut().push(format!("ranges {memory}"));
            Ok(vec![memory as u8])
        }
    }

    fn session() -> (Session<Mock>, Rc<RefCell<Vec<String>>>) {
        let m = Mock::default();
        let log = m.log.clone();
        (Session::new(m, b"tok".to_vec()), log)
    }

    fn req_frame(seq: u16, epoch: u32, r: &Request) -> Vec<u8> {
        Frame::new(STREAM_CONTROL, flags::CRC, seq, epoch, r.encode())
            .unwrap()
            .encode()
    }

    fn hello() -> Request {
        Request::Hello {
            version: PROTOCOL_VERSION,
            token: b"tok".to_vec(),
        }
    }

    fn parse_responses(bytes: &[u8]) -> Vec<(Frame, Response)> {
        let mut fr = FrameReader::new();
        fr.push(bytes);
        let mut out = Vec::new();
        while let Some(f) = fr.next_frame().unwrap() {
            let r = Response::decode(&f.payload).unwrap();
            out.push((f, r));
        }
        assert_eq!(fr.buffered(), 0, "bajty nadmiarowe w wyjściu");
        out
    }

    /// Czytnik oddający po jednym bajcie, żeby sprawdzić składanie ramek z kawałków.
    struct OneByte(Cursor<Vec<u8>>);
    impl Read for OneByte {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(1);
            self.0.read(&mut buf[..n])
        }
    }

    struct FailingWriter;
    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "x"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn hello_then_query_get_echoed_seq_epoch_and_clean_eof_is_ok() {
        let (mut s, _) = session();
        let mut input = req_frame(7, 42, &hello());
        input.extend(req_frame(8, 42, &Request::EnumInstanceVersion));
        let mut out = Vec::new();
        assert_eq!(
            serve_connection(&mut s, &mut Cursor::new(input), &mut out),
            Ok(())
        );
        let r = parse_responses(&out);
        assert_eq!(r.len(), 2);
        assert_eq!(
            r[0].1,
            Response::HelloOk {
                version: PROTOCOL_VERSION
            }
        );
        assert_eq!(
            (r[0].0.header.seq, r[0].0.header.epoch, r[0].0.header.stream),
            (7, 42, STREAM_CONTROL)
        );
        assert_eq!(r[1].1, Response::U32(0x0040_3000));
        assert_eq!((r[1].0.header.seq, r[1].0.header.epoch), (8, 42));
        assert_ne!(r[1].0.header.flags & flags::CRC, 0, "odpowiedzi mają CRC");
    }

    #[test]
    fn consecutive_memory_delta_requests_are_prefetched_as_one_group_and_answered_in_order() {
        let (mut s, log) = session();
        let mut input = req_frame(1, 0, &hello());
        input.extend(req_frame(2, 0, &Request::EnumInstanceVersion));
        input.extend(req_frame(3, 0, &Request::MemReadDelta { memory: 11 }));
        input.extend(req_frame(4, 0, &Request::MemReadDeltaRanges { memory: 12, ranges: vec![(0, 4096)], cached: false }));
        input.extend(req_frame(5, 0, &Request::MemReadDelta { memory: 13 }));
        input.extend(req_frame(6, 0, &Request::EnumInstanceVersion));
        // Pojedyncze żądanie po innym poleceniu nie tworzy grupy.
        input.extend(req_frame(7, 0, &Request::MemReadDelta { memory: 14 }));
        let mut out = Vec::new();
        assert_eq!(serve_connection(&mut s, &mut Cursor::new(input), &mut out), Ok(()));
        let r = parse_responses(&out);
        assert_eq!(r.iter().map(|(f, _)| f.header.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6, 7], "kolejność odpowiedzi");
        assert_eq!(r[2].1, Response::Data(vec![11]));
        assert_eq!(r[3].1, Response::Data(vec![12]));
        assert_eq!(r[4].1, Response::Data(vec![13]));
        assert_eq!(r[6].1, Response::Data(vec![14]));
        let l = log.borrow().clone();
        let pos = |x: &str| l.iter().position(|e| e == x).unwrap_or_else(|| panic!("brak {x} w {l:?}"));
        assert_eq!(l.iter().filter(|e| e.starts_with("prefetch ")).count(), 1, "tylko jedna grupa: {l:?}");
        assert!(pos("prefetch [11, 12, 13]") < pos("delta 11") && pos("delta 11") < pos("ranges 12") && pos("ranges 12") < pos("delta 13"));
        assert!(pos("delta 13") < pos("prefetch_end") && pos("prefetch_end") < pos("delta 14"), "koniec grupy przed następnym żądaniem: {l:?}");
    }

    /// Zapisy do gniazda liczone osobno, żeby sprawdzić, że odpowiedzi na grupę żądań idą razem.
    struct CountingWriter {
        bytes: Vec<u8>,
        writes: usize,
    }
    impl Write for CountingWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn responses_to_requests_read_together_are_written_together_in_order() {
        let (mut s, _) = session();
        let mut input = req_frame(1, 0, &hello());
        for seq in 2..=6 {
            input.extend(req_frame(seq, 0, &Request::EnumInstanceVersion));
        }
        let mut out = CountingWriter { bytes: Vec::new(), writes: 0 };
        assert_eq!(serve_connection(&mut s, &mut Cursor::new(input), &mut out), Ok(()));
        let r = parse_responses(&out.bytes);
        assert_eq!(r.len(), 6);
        assert_eq!(r.iter().map(|(f, _)| f.header.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6], "kolejność zachowana");
        assert_eq!(out.writes, 1, "sześć odpowiedzi z jednego odczytu to jeden zapis do gniazda");
    }

    #[test]
    fn responses_before_a_failure_are_still_delivered() {
        // Hello z błędnym tokenem: odpowiedź błędu musi dojść mimo zwrotu AuthFailed (opróżnienie zaległych odpowiedzi).
        let (mut s, _) = session();
        let bad = Request::Hello { version: PROTOCOL_VERSION, token: b"zly".to_vec() };
        let mut out = CountingWriter { bytes: Vec::new(), writes: 0 };
        assert_eq!(serve_connection(&mut s, &mut Cursor::new(req_frame(1, 0, &bad)), &mut out), Err(ServeError::AuthFailed));
        assert_eq!(parse_responses(&out.bytes).len(), 1);
    }

    #[test]
    fn frames_split_into_single_bytes_are_reassembled() {
        let (mut s, _) = session();
        let mut input = req_frame(1, 0, &hello());
        input.extend(req_frame(2, 0, &Request::EnumInstanceVersion));
        let mut out = Vec::new();
        assert_eq!(
            serve_connection(&mut s, &mut OneByte(Cursor::new(input)), &mut out),
            Ok(())
        );
        assert_eq!(parse_responses(&out).len(), 2);
    }

    #[test]
    fn empty_input_is_a_clean_close() {
        let (mut s, _) = session();
        let mut out = Vec::new();
        assert_eq!(
            serve_connection(&mut s, &mut Cursor::new(Vec::new()), &mut out),
            Ok(())
        );
        assert!(out.is_empty());
    }

    #[test]
    fn wrong_token_sends_error_then_auth_failed_and_ignores_later_frames() {
        let (mut s, _) = session();
        let mut input = req_frame(
            1,
            0,
            &Request::Hello {
                version: PROTOCOL_VERSION,
                token: b"zly".to_vec(),
            },
        );
        input.extend(req_frame(2, 0, &Request::EnumInstanceVersion));
        let mut out = Vec::new();
        assert_eq!(
            serve_connection(&mut s, &mut Cursor::new(input), &mut out),
            Err(ServeError::AuthFailed)
        );
        let r = parse_responses(&out);
        assert_eq!(
            r.len(),
            1,
            "po nieudanym Hello nic więcej nie jest obsługiwane"
        );
        assert!(matches!(r[0].1, Response::Error { .. }));
    }

    #[test]
    fn request_before_hello_is_rejected_and_closes() {
        let (mut s, _) = session();
        let mut out = Vec::new();
        let input = req_frame(1, 0, &Request::EnumInstanceVersion);
        assert_eq!(
            serve_connection(&mut s, &mut Cursor::new(input), &mut out),
            Err(ServeError::AuthFailed)
        );
        assert_eq!(parse_responses(&out).len(), 1);
    }

    #[test]
    fn undecodable_payload_gets_protocol_error_then_msg_error() {
        let (mut s, _) = session();
        let mut input = req_frame(1, 0, &hello());
        input.extend(
            Frame::new(STREAM_CONTROL, flags::CRC, 2, 0, vec![200])
                .unwrap()
                .encode(),
        );
        let mut out = Vec::new();
        let res = serve_connection(&mut s, &mut Cursor::new(input), &mut out);
        assert!(matches!(res, Err(ServeError::Msg(_))), "{res:?}");
        let r = parse_responses(&out);
        assert_eq!(r.len(), 2);
        assert!(matches!(&r[1].1, Response::Error { code, .. } if *code == ERR_PROTOCOL));
        assert_eq!(r[1].0.header.seq, 2);
    }

    #[test]
    fn bad_crc_and_reserved_flags_are_frame_errors_without_reply() {
        let (mut s, _) = session();
        let mut bad = req_frame(1, 0, &hello());
        let last = bad.len() - 1;
        bad[last] ^= 0xFF; // psuje payload => zły CRC
        let mut out = Vec::new();
        let res = serve_connection(&mut s, &mut Cursor::new(bad), &mut out);
        assert!(
            matches!(res, Err(ServeError::Frame(FrameError::BadCrc { .. }))),
            "{res:?}"
        );
        assert!(out.is_empty());

        let (mut s, _) = session();
        let mut rsv = req_frame(1, 0, &hello());
        rsv[5] |= 0x80; // zarezerwowany bit flag
        let res = serve_connection(&mut s, &mut Cursor::new(rsv), &mut Vec::new());
        assert!(
            matches!(res, Err(ServeError::Frame(FrameError::UnknownFlags(_)))),
            "{res:?}"
        );
    }

    #[test]
    fn eof_in_the_middle_of_a_frame_is_unexpected_eof() {
        let (mut s, _) = session();
        let full = req_frame(1, 0, &hello());
        for cut in [1, 15, 16, full.len() - 1] {
            let (mut s2, _) = session();
            let res = serve_connection(
                &mut s2,
                &mut Cursor::new(full[..cut].to_vec()),
                &mut Vec::new(),
            );
            assert_eq!(
                res,
                Err(ServeError::Io(std::io::ErrorKind::UnexpectedEof)),
                "cięcie {cut}"
            );
        }
        let _ = &mut s;
    }

    #[test]
    fn other_stream_or_zstd_flag_is_unsupported() {
        let (mut s, _) = session();
        let other = Frame::new(5, flags::CRC, 1, 0, hello().encode())
            .unwrap()
            .encode();
        assert_eq!(
            serve_connection(&mut s, &mut Cursor::new(other), &mut Vec::new()),
            Err(ServeError::Unsupported)
        );
        let (mut s, _) = session();
        let z = Frame::new(
            STREAM_CONTROL,
            flags::CRC | flags::ZSTD,
            1,
            0,
            hello().encode(),
        )
        .unwrap()
        .encode();
        assert_eq!(
            serve_connection(&mut s, &mut Cursor::new(z), &mut Vec::new()),
            Err(ServeError::Unsupported)
        );
    }

    #[test]
    fn attach_video_turns_the_connection_into_a_frame_stream_that_ends_with_the_control_session() {
        let (mut control, _) = session();
        control.handle_request(hello());
        let Some(Response::Handle(id)) = control.handle_request(Request::SessionId) else { panic!() };
        control.handle_request(Request::PresentFrame { memory: 7, len: 4, seq: 3, width: 1, height: 1, format: 1, codec: 0 });
        drop(control); // sesja sterująca kończy się: kanał się zamyka po opróżnieniu
        let (mut video, _) = session();
        let mut input = req_frame(1, 0, &hello());
        input.extend(req_frame(2, 0, &Request::AttachVideo { session: id }));
        let mut out = Vec::new();
        assert_eq!(serve_connection(&mut video, &mut Cursor::new(input), &mut out), Ok(()));
        let mut fr = FrameReader::new();
        fr.push(&out);
        let a = fr.next_frame().unwrap().unwrap();
        assert_eq!(Response::decode(&a.payload).unwrap(), Response::HelloOk { version: PROTOCOL_VERSION });
        let b = fr.next_frame().unwrap().unwrap();
        assert_eq!(Response::decode(&b.payload).unwrap(), Response::U32(0), "odpowiedź na AttachVideo");
        let c = fr.next_frame().unwrap().unwrap();
        assert_eq!(c.header.stream, STREAM_VIDEO);
        assert_eq!(c.header.flags & flags::CRC, 0);
        let h = gpu_proto::video::VideoHeader::decode(&c.payload[..28]).unwrap();
        assert_eq!((h.seq, h.width, h.height), (3, 1, 1));
        assert!(fr.next_frame().unwrap().is_none());
    }

    #[test]
    fn attach_video_to_an_unknown_or_already_claimed_session_is_an_error_reply() {
        let (mut v, _) = session();
        let mut input = req_frame(1, 0, &hello());
        input.extend(req_frame(2, 0, &Request::AttachVideo { session: 0xBAD }));
        let mut out = Vec::new();
        assert_eq!(serve_connection(&mut v, &mut Cursor::new(input), &mut out), Ok(()));
        let r = parse_responses(&out);
        assert!(matches!(r[1].1, Response::Error { .. }));
    }

    #[test]
    fn attach_video_before_hello_is_rejected_and_closes() {
        let (mut v, _) = session();
        let mut out = Vec::new();
        let input = req_frame(1, 0, &Request::AttachVideo { session: 1 });
        assert_eq!(serve_connection(&mut v, &mut Cursor::new(input), &mut out), Err(ServeError::AuthFailed));
    }

    #[test]
    fn cleanup_runs_on_every_exit_path_exactly_once() {
        // Normalne zakończenie.
        let (mut s, log) = session();
        let mut input = req_frame(1, 0, &hello());
        input.extend(req_frame(
            2,
            0,
            &Request::CreateInstance {
                api_version: 1,
                extensions: vec![],
            },
        ));
        serve_connection(&mut s, &mut Cursor::new(input.clone()), &mut Vec::new()).unwrap();
        assert_eq!(
            log.borrow()
                .iter()
                .filter(|l| l.starts_with("destroy"))
                .count(),
            1,
            "{:?}",
            log.borrow()
        );
        // Błąd pisania.
        let (mut s, log) = session();
        let res = serve_connection(&mut s, &mut Cursor::new(input.clone()), &mut FailingWriter);
        assert_eq!(res, Err(ServeError::Io(std::io::ErrorKind::BrokenPipe)));
        assert!(
            log.borrow()
                .iter()
                .filter(|l| l.starts_with("destroy"))
                .count()
                <= 1
        );
        // Ucięcie w środku ramki po utworzeniu instancji.
        let (mut s, log) = session();
        let mut cut = input.clone();
        cut.truncate(cut.len() - 3);
        let _ = serve_connection(&mut s, &mut Cursor::new(cut), &mut Vec::new());
        assert!(
            log.borrow()
                .iter()
                .filter(|l| l.starts_with("destroy"))
                .count()
                <= 1
        );
    }

    #[test]
    fn instance_created_before_a_late_error_is_destroyed() {
        let (mut s, log) = session();
        let mut input = req_frame(1, 0, &hello());
        input.extend(req_frame(
            2,
            0,
            &Request::CreateInstance {
                api_version: 1,
                extensions: vec![],
            },
        ));
        input.extend(
            Frame::new(STREAM_CONTROL, flags::CRC, 3, 0, vec![250])
                .unwrap()
                .encode(),
        );
        let res = serve_connection(&mut s, &mut Cursor::new(input), &mut Vec::new());
        assert!(matches!(res, Err(ServeError::Msg(_))));
        assert_eq!(log.borrow().last().unwrap(), "destroy 500");
    }

    #[test]
    fn random_garbage_never_panics() {
        let mut x: u64 = 0xDEAD_BEEF_1234_5678;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..500 {
            let len = (next() % 200) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let (mut s, _) = session();
            let _ = serve_connection(&mut s, &mut Cursor::new(bytes), &mut Vec::new());
        }
    }
}
