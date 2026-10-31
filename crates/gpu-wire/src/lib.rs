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
        todo!()
    }
    pub fn with_capacity(capacity: usize) -> Self {
        todo!()
    }
    pub fn put_u8(&mut self, v: u8) {
        todo!()
    }
    pub fn put_u16(&mut self, v: u16) {
        todo!()
    }
    pub fn put_u32(&mut self, v: u32) {
        todo!()
    }
    pub fn put_u64(&mut self, v: u64) {
        todo!()
    }
    pub fn put_f32(&mut self, v: f32) {
        todo!()
    }
    pub fn put_f64(&mut self, v: f64) {
        todo!()
    }
    /// LEB128 bez znaku.
    pub fn put_varint(&mut self, v: u64) {
        todo!()
    }
    /// Długość (varint) i surowe bajty.
    pub fn put_bytes(&mut self, v: &[u8]) {
        todo!()
    }
    /// Długość (varint) i bajty UTF-8.
    pub fn put_str(&mut self, v: &str) {
        todo!()
    }
    pub fn len(&self) -> usize {
        todo!()
    }
    pub fn is_empty(&self) -> bool {
        todo!()
    }
    pub fn finish(self) -> Vec<u8> {
        todo!()
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
        todo!()
    }
    pub fn get_u8(&mut self) -> Result<u8, WireError> {
        todo!()
    }
    pub fn get_u16(&mut self) -> Result<u16, WireError> {
        todo!()
    }
    pub fn get_u32(&mut self) -> Result<u32, WireError> {
        todo!()
    }
    pub fn get_u64(&mut self) -> Result<u64, WireError> {
        todo!()
    }
    pub fn get_f32(&mut self) -> Result<f32, WireError> {
        todo!()
    }
    pub fn get_f64(&mut self) -> Result<f64, WireError> {
        todo!()
    }
    pub fn get_varint(&mut self) -> Result<u64, WireError> {
        todo!()
    }
    pub fn get_bytes(&mut self) -> Result<&'a [u8], WireError> {
        todo!()
    }
    pub fn get_str(&mut self) -> Result<&'a str, WireError> {
        todo!()
    }
    /// Liczba nieprzeczytanych bajtów.
    pub fn remaining(&self) -> usize {
        todo!()
    }
    pub fn is_empty(&self) -> bool {
        todo!()
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
