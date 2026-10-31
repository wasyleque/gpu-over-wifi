//! Prymitywy formatu przewodowego zdalnego GPU: kodowanie i dekodowanie wartości.
//!
//! Zasady formatu: liczby stałej szerokości są little-endian; długości i liczby bez znaku
//! o zmiennej szerokości używają LEB128 (max 10 bajtów); bajty i napisy mają prefiks długości.

/// Błędy dekodowania.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// Zabrakło bajtów w buforze.
    UnexpectedEof,
    /// Napis nie jest poprawnym UTF-8.
    InvalidUtf8,
    /// Liczba LEB128 nie mieści się w u64.
    VarintOverflow,
}

/// Koduje wartości do bufora bajtów.
#[derive(Debug, Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }
    pub fn put_u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn put_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// LEB128 bez znaku.
    pub fn put_varint(&mut self, v: u64) {
        let mut value = v;
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                self.buf.push(byte);
                break;
            } else {
                self.buf.push(byte | 0x80);
            }
        }
    }
    /// Długość (varint) i surowe bajty.
    pub fn put_bytes(&mut self, v: &[u8]) {
        self.put_varint(v.len() as u64);
        self.buf.extend_from_slice(v);
    }
    /// Długość (varint) i bajty UTF-8.
    pub fn put_str(&mut self, v: &str) {
        self.put_bytes(v.as_bytes());
    }
    pub fn len(&self) -> usize {
        self.buf.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// Dekoduje wartości z bufora bajtów; nie przesuwa pozycji przy błędzie.
#[derive(Debug)]
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn get_u8(&mut self) -> Result<u8, WireError> {
        if self.pos + 1 > self.buf.len() {
            return Err(WireError::UnexpectedEof);
        }
        let value = self.buf[self.pos];
        self.pos += 1;
        Ok(value)
    }
    pub fn get_u16(&mut self) -> Result<u16, WireError> {
        if self.pos + 2 > self.buf.len() {
            return Err(WireError::UnexpectedEof);
        }
        let value = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(value)
    }
    pub fn get_u32(&mut self) -> Result<u32, WireError> {
        if self.pos + 4 > self.buf.len() {
            return Err(WireError::UnexpectedEof);
        }
        let value = u32::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1], self.buf[self.pos + 2], self.buf[self.pos + 3]]);
        self.pos += 4;
        Ok(value)
    }
    pub fn get_u64(&mut self) -> Result<u64, WireError> {
        if self.pos + 8 > self.buf.len() {
            return Err(WireError::UnexpectedEof);
        }
        let value = u64::from_le_bytes([
            self.buf[self.pos], self.buf[self.pos + 1], self.buf[self.pos + 2], self.buf[self.pos + 3],
            self.buf[self.pos + 4], self.buf[self.pos + 5], self.buf[self.pos + 6], self.buf[self.pos + 7]
        ]);
        self.pos += 8;
        Ok(value)
    }
    pub fn get_f32(&mut self) -> Result<f32, WireError> {
        if self.pos + 4 > self.buf.len() {
            return Err(WireError::UnexpectedEof);
        }
        let value = f32::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1], self.buf[self.pos + 2], self.buf[self.pos + 3]]);
        self.pos += 4;
        Ok(value)
    }
    pub fn get_f64(&mut self) -> Result<f64, WireError> {
        if self.pos + 8 > self.buf.len() {
            return Err(WireError::UnexpectedEof);
        }
        let value = f64::from_le_bytes([
            self.buf[self.pos], self.buf[self.pos + 1], self.buf[self.pos + 2], self.buf[self.pos + 3],
            self.buf[self.pos + 4], self.buf[self.pos + 5], self.buf[self.pos + 6], self.buf[self.pos + 7]
        ]);
        self.pos += 8;
        Ok(value)
    }
    pub fn get_varint(&mut self) -> Result<u64, WireError> {
        let mut value: u64 = 0;
        let mut shift = 0;
        let original_pos = self.pos;
        
        loop {
            if self.pos >= self.buf.len() {
                self.pos = original_pos;
                return Err(WireError::UnexpectedEof);
            }
            
            let byte = self.buf[self.pos];
            self.pos += 1;
            
            // Check for overflow when we're at the 10th byte (shift == 63)
            if shift == 63 {
                // The 10th byte may only have bits 0 and 1 set (0x00 or 0x01)
                if (byte & 0x7E) != 0 {
                    self.pos = original_pos;
                    return Err(WireError::VarintOverflow);
                }
            }
            
            value |= ((byte & 0x7F) as u64) << shift;
            
            if (byte & 0x80) == 0 {
                break;
            }
            
            shift += 7;
            
            // Check for overflow - LEB128 numbers must fit in 10 bytes
            if shift >= 64 {
                self.pos = original_pos;
                return Err(WireError::VarintOverflow);
            }
        }
        
        Ok(value)
    }
    pub fn get_bytes(&mut self) -> Result<&'a [u8], WireError> {
        let original_pos = self.pos;
        let len = self.get_varint()?;
        
        if self.pos + len as usize > self.buf.len() {
            self.pos = original_pos;
            return Err(WireError::UnexpectedEof);
        }
        
        let start = self.pos;
        self.pos += len as usize;
        Ok(&self.buf[start..self.pos])
    }
    pub fn get_str(&mut self) -> Result<&'a str, WireError> {
        let bytes = self.get_bytes()?;
        std::str::from_utf8(bytes).map_err(|_| WireError::InvalidUtf8)
    }
    /// Liczba nieprzeczytanych bajtów.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_width_values_roundtrip_little_endian() {
        let mut e = Encoder::new();
        e.put_u8(0xAB);
        e.put_u16(0x1234);
        e.put_u32(0xDEADBEEF);
        e.put_u64(0x0102030405060708);
        e.put_f32(1.5);
        e.put_f64(-2.25);
        let bytes = e.finish();
        assert_eq!(&bytes[..3], &[0xAB, 0x34, 0x12]);
        assert_eq!(&bytes[3..7], &[0xEF, 0xBE, 0xAD, 0xDE]);
        let mut d = Decoder::new(&bytes);
        assert_eq!(d.get_u8().unwrap(), 0xAB);
        assert_eq!(d.get_u16().unwrap(), 0x1234);
        assert_eq!(d.get_u32().unwrap(), 0xDEADBEEF);
        assert_eq!(d.get_u64().unwrap(), 0x0102030405060708);
        assert_eq!(d.get_f32().unwrap(), 1.5);
        assert_eq!(d.get_f64().unwrap(), -2.25);
        assert!(d.is_empty());
        assert_eq!(d.remaining(), 0);
    }

    #[test]
    fn float_special_values_keep_their_bits() {
        let mut e = Encoder::new();
        e.put_f32(f32::NAN);
        e.put_f64(f64::NEG_INFINITY);
        e.put_f32(-0.0);
        let b = e.finish();
        let mut d = Decoder::new(&b);
        assert!(d.get_f32().unwrap().is_nan());
        assert_eq!(d.get_f64().unwrap(), f64::NEG_INFINITY);
        assert!(d.get_f32().unwrap().is_sign_negative());
    }

    #[test]
    fn varint_known_encodings() {
        let cases: &[(u64, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7F]),
            (128, &[0x80, 0x01]),
            (300, &[0xAC, 0x02]),
            (16384, &[0x80, 0x80, 0x01]),
        ];
        for (value, expected) in cases {
            let mut e = Encoder::new();
            e.put_varint(*value);
            assert_eq!(e.finish(), *expected, "kodowanie {value}");
            let mut d = Decoder::new(expected);
            assert_eq!(d.get_varint().unwrap(), *value);
            assert!(d.is_empty());
        }
    }

    #[test]
    fn varint_u64_max_takes_ten_bytes_and_roundtrips() {
        let mut e = Encoder::new();
        e.put_varint(u64::MAX);
        let b = e.finish();
        assert_eq!(b.len(), 10);
        assert_eq!(b[9], 0x01);
        assert_eq!(Decoder::new(&b).get_varint().unwrap(), u64::MAX);
    }

    #[test]
    fn varint_overflow_is_rejected() {
        // 11 bajtów z ustawionym bitem kontynuacji.
        let too_long = [0x80u8; 10].iter().copied().chain([0x01]).collect::<Vec<_>>();
        assert_eq!(Decoder::new(&too_long).get_varint(), Err(WireError::VarintOverflow));
        // 10. bajt z bitami powyżej 64. bitu.
        let mut high = [0xFFu8; 9].to_vec();
        high.push(0x02);
        assert_eq!(Decoder::new(&high).get_varint(), Err(WireError::VarintOverflow));
    }

    #[test]
    fn truncated_varint_is_eof() {
        assert_eq!(Decoder::new(&[0x80]).get_varint(), Err(WireError::UnexpectedEof));
        assert_eq!(Decoder::new(&[]).get_varint(), Err(WireError::UnexpectedEof));
    }

    #[test]
    fn bytes_and_strings_are_length_prefixed() {
        let mut e = Encoder::new();
        e.put_bytes(&[1, 2, 3]);
        e.put_str("zażółć");
        e.put_bytes(&[]);
        let b = e.finish();
        assert_eq!(&b[..4], &[3, 1, 2, 3]);
        let mut d = Decoder::new(&b);
        assert_eq!(d.get_bytes().unwrap(), &[1, 2, 3]);
        assert_eq!(d.get_str().unwrap(), "zażółć");
        assert_eq!(d.get_bytes().unwrap(), &[] as &[u8]);
        assert!(d.is_empty());
    }

    #[test]
    fn invalid_utf8_is_reported() {
        let mut e = Encoder::new();
        e.put_bytes(&[0xFF, 0xFE]);
        let b = e.finish();
        assert_eq!(Decoder::new(&b).get_str(), Err(WireError::InvalidUtf8));
    }

    #[test]
    fn eof_errors_and_decoder_does_not_advance_on_failure() {
        let mut d = Decoder::new(&[1, 2, 3]);
        assert_eq!(d.get_u32(), Err(WireError::UnexpectedEof));
        assert_eq!(d.remaining(), 3, "pozycja nie może się przesunąć po błędzie");
        assert_eq!(d.get_u16().unwrap(), 0x0201);
        assert_eq!(d.get_u16(), Err(WireError::UnexpectedEof));
        assert_eq!(d.remaining(), 1);
        // Długość większa niż reszta bufora.
        let mut e = Encoder::new();
        e.put_varint(100);
        e.put_u8(7);
        let b = e.finish();
        let mut d2 = Decoder::new(&b);
        assert_eq!(d2.get_bytes(), Err(WireError::UnexpectedEof));
        assert_eq!(d2.remaining(), b.len(), "get_bytes też nie przesuwa pozycji");
    }

    #[test]
    fn encoder_reports_length() {
        let mut e = Encoder::with_capacity(16);
        assert!(e.is_empty());
        e.put_u32(1);
        assert_eq!(e.len(), 4);
        assert!(!e.is_empty());
    }
}
