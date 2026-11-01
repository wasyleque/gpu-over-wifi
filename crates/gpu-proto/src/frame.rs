//! Ramka protokołu: nagłówek 16 B (little-endian) i payload.
//!
//! ```text
//! offset size field
//! 0      4    len     (u32, długość payloadu, <= MAX_FRAME)
//! 4      1    stream  (u8)
//! 5      1    flags   (u8, patrz `flags`)
//! 6      2    seq     (u16)
//! 8      4    epoch   (u32)
//! 12     4    crc32c  (u32, CRC32C payloadu; 0 gdy flaga CRC wyłączona)
//! 16     len  payload
//! ```
//! Dane z sieci są niezaufane: długość jest sprawdzana PRZED alokacją payloadu.

pub const HEADER_LEN: usize = 16;
/// Maksymalna długość payloadu jednej ramki (16 MiB).
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

pub mod flags {
    pub const ZSTD: u8 = 1 << 0;
    pub const LAST_IN_BATCH: u8 = 1 << 1;
    pub const ACK_REQ: u8 = 1 << 2;
    pub const CRC: u8 = 1 << 3;
    /// Suma wszystkich znanych flag; pozostałe bity są zarezerwowane.
    pub const ALL: u8 = ZSTD | LAST_IN_BATCH | ACK_REQ | CRC;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Zadeklarowana długość payloadu przekracza `MAX_FRAME`.
    TooLarge(u32),
    /// Ustawiono zarezerwowane bity flag.
    UnknownFlags(u8),
    /// Flaga CRC ustawiona, a suma nie zgadza się z payloadem.
    BadCrc { expected: u32, actual: u32 },
    /// Czytnik jest zablokowany po wcześniejszym błędzie.
    Poisoned,
}

/// CRC32C (Castagnoli, wielomian odwrócony 0x82F63B78), implementacja programowa.
pub fn crc32c(data: &[u8]) -> u32 {
    todo!()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub len: u32,
    pub stream: u8,
    pub flags: u8,
    pub seq: u16,
    pub epoch: u32,
    pub crc32c: u32,
}

impl FrameHeader {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        todo!()
    }

    /// Dekoduje nagłówek i waliduje `len` oraz `flags`.
    pub fn decode(bytes: &[u8; HEADER_LEN]) -> Result<Self, FrameError> {
        todo!()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: FrameHeader,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Buduje ramkę: ustawia `len`, a przy fladze CRC liczy `crc32c`.
    pub fn new(stream: u8, flags: u8, seq: u16, epoch: u32, payload: Vec<u8>) -> Result<Frame, FrameError> {
        todo!()
    }

    /// Nagłówek + payload.
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }
}

/// Składa ramki z dowolnie pociętego strumienia bajtów.
#[derive(Debug, Default)]
pub struct FrameReader {
    buf: Vec<u8>,
    poisoned: bool,
}

impl FrameReader {
    pub fn new() -> Self {
        todo!()
    }

    /// Dopisuje odebrane bajty.
    pub fn push(&mut self, data: &[u8]) {
        todo!()
    }

    /// `Ok(Some)` gdy jest kompletna ramka, `Ok(None)` gdy trzeba więcej danych.
    /// Po pierwszym błędzie czytnik jest zablokowany i zawsze zwraca `Err(Poisoned)`.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        todo!()
    }

    /// Liczba zbuforowanych, jeszcze nieprzetworzonych bajtów.
    pub fn buffered(&self) -> usize {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_known_vectors() {
        assert_eq!(crc32c(b""), 0);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(&[0u8; 32]), 0x8A91_36AA);
    }

    #[test]
    fn header_layout_is_little_endian_and_16_bytes() {
        let h = FrameHeader { len: 0x0102_0304, stream: 7, flags: flags::ZSTD | flags::CRC, seq: 0x0A0B, epoch: 0x1122_3344, crc32c: 0xAABB_CCDD };
        let b = h.encode();
        assert_eq!(b, [0x04, 0x03, 0x02, 0x01, 7, 0b1001, 0x0B, 0x0A, 0x44, 0x33, 0x22, 0x11, 0xDD, 0xCC, 0xBB, 0xAA]);
    }

    #[test]
    fn header_roundtrip() {
        let h = FrameHeader { len: 1000, stream: 3, flags: flags::ALL, seq: 65535, epoch: u32::MAX, crc32c: 5 };
        assert_eq!(FrameHeader::decode(&h.encode()).unwrap(), h);
    }

    #[test]
    fn decode_rejects_oversized_length_without_allocating() {
        let mut h = FrameHeader { len: MAX_FRAME + 1, stream: 0, flags: 0, seq: 0, epoch: 0, crc32c: 0 };
        assert_eq!(FrameHeader::decode(&h.encode()), Err(FrameError::TooLarge(MAX_FRAME + 1)));
        h.len = u32::MAX;
        assert_eq!(FrameHeader::decode(&h.encode()), Err(FrameError::TooLarge(u32::MAX)));
        h.len = MAX_FRAME;
        assert!(FrameHeader::decode(&h.encode()).is_ok());
    }

    #[test]
    fn decode_rejects_reserved_flag_bits() {
        let h = FrameHeader { len: 0, stream: 0, flags: 0b0001_0000, seq: 0, epoch: 0, crc32c: 0 };
        assert_eq!(FrameHeader::decode(&h.encode()), Err(FrameError::UnknownFlags(0b0001_0000)));
        let h = FrameHeader { flags: 0xF0 | flags::CRC, ..h };
        assert_eq!(FrameHeader::decode(&h.encode()), Err(FrameError::UnknownFlags(0xF0)));
    }

    #[test]
    fn frame_new_sets_len_and_crc_only_when_flagged() {
        let f = Frame::new(1, flags::CRC, 9, 2, b"123456789".to_vec()).unwrap();
        assert_eq!(f.header.len, 9);
        assert_eq!(f.header.crc32c, 0xE306_9283);
        let g = Frame::new(1, 0, 9, 2, b"123456789".to_vec()).unwrap();
        assert_eq!(g.header.crc32c, 0);
        assert!(Frame::new(1, 0x80, 0, 0, vec![]).is_err(), "zarezerwowane flagi odrzucone już przy budowie");
    }

    #[test]
    fn frame_new_rejects_oversized_payload() {
        let big = vec![0u8; MAX_FRAME as usize + 1];
        assert_eq!(Frame::new(0, 0, 0, 0, big), Err(FrameError::TooLarge(MAX_FRAME + 1)));
    }

    #[test]
    fn reader_roundtrip_whole_buffer() {
        let f = Frame::new(2, flags::CRC | flags::LAST_IN_BATCH, 1, 1, vec![1, 2, 3, 4, 5]).unwrap();
        let mut r = FrameReader::new();
        r.push(&f.encode());
        assert_eq!(r.next_frame().unwrap(), Some(f));
        assert_eq!(r.next_frame().unwrap(), None);
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn reader_handles_byte_by_byte_and_multiple_frames() {
        let a = Frame::new(1, flags::CRC, 1, 1, vec![9; 100]).unwrap();
        let b = Frame::new(2, 0, 2, 1, vec![]).unwrap();
        let c = Frame::new(3, flags::ZSTD, 3, 1, vec![7; 5]).unwrap();
        let mut wire = a.encode();
        wire.extend(b.encode());
        wire.extend(c.encode());
        let mut r = FrameReader::new();
        let mut got = Vec::new();
        for byte in &wire {
            r.push(std::slice::from_ref(byte));
            while let Some(f) = r.next_frame().unwrap() {
                got.push(f);
            }
        }
        assert_eq!(got, vec![a, b, c]);
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn reader_returns_two_frames_pushed_at_once() {
        let a = Frame::new(1, 0, 1, 1, vec![1]).unwrap();
        let b = Frame::new(1, 0, 2, 1, vec![2]).unwrap();
        let mut wire = a.encode();
        wire.extend(b.encode());
        let mut r = FrameReader::new();
        r.push(&wire);
        assert_eq!(r.next_frame().unwrap(), Some(a));
        assert_eq!(r.next_frame().unwrap(), Some(b));
        assert_eq!(r.next_frame().unwrap(), None);
    }

    #[test]
    fn reader_waits_on_partial_header_and_partial_payload() {
        let f = Frame::new(1, 0, 1, 1, vec![5; 10]).unwrap();
        let w = f.encode();
        let mut r = FrameReader::new();
        r.push(&w[..7]);
        assert_eq!(r.next_frame().unwrap(), None);
        r.push(&w[7..20]);
        assert_eq!(r.next_frame().unwrap(), None);
        r.push(&w[20..]);
        assert_eq!(r.next_frame().unwrap(), Some(f));
    }

    #[test]
    fn reader_rejects_oversized_length_as_soon_as_header_arrives() {
        // Atak: nagłówek deklaruje 4 GiB. Błąd musi wyjść od razu, bez czekania na payload.
        let h = FrameHeader { len: u32::MAX, stream: 0, flags: 0, seq: 0, epoch: 0, crc32c: 0 };
        let mut r = FrameReader::new();
        r.push(&h.encode());
        assert_eq!(r.next_frame(), Err(FrameError::TooLarge(u32::MAX)));
    }

    #[test]
    fn reader_detects_bad_crc_and_is_then_poisoned() {
        let mut f = Frame::new(1, flags::CRC, 1, 1, vec![1, 2, 3]).unwrap();
        f.payload[1] ^= 0xFF;
        let mut r = FrameReader::new();
        r.push(&f.encode());
        match r.next_frame() {
            Err(FrameError::BadCrc { .. }) => {}
            other => panic!("oczekiwano BadCrc, jest {other:?}"),
        }
        // Po błędzie strumień jest niewiarygodny: nawet poprawna ramka nie może przejść.
        let ok = Frame::new(1, 0, 2, 1, vec![]).unwrap();
        r.push(&ok.encode());
        assert_eq!(r.next_frame(), Err(FrameError::Poisoned));
    }

    #[test]
    fn reader_ignores_crc_field_when_flag_is_off() {
        let mut f = Frame::new(1, 0, 1, 1, vec![1, 2, 3]).unwrap();
        f.header.crc32c = 0xDEAD_BEEF; // bez flagi CRC pole jest ignorowane
        let mut r = FrameReader::new();
        r.push(&f.encode());
        assert_eq!(r.next_frame().unwrap().unwrap().payload, vec![1, 2, 3]);
    }

    #[test]
    fn reader_unknown_flags_poison() {
        let h = FrameHeader { len: 0, stream: 0, flags: 0x40, seq: 0, epoch: 0, crc32c: 0 };
        let mut r = FrameReader::new();
        r.push(&h.encode());
        assert_eq!(r.next_frame(), Err(FrameError::UnknownFlags(0x40)));
        assert_eq!(r.next_frame(), Err(FrameError::Poisoned));
    }
}
