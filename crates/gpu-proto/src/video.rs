//! Ramki obrazu serwer -> klient (M2b: surowe piksele; M5: HEVC): nagłówek, potwierdzenie i pakowanie surowych wierszy.
//!
//! Nagłówek ma stały układ 28 B, little-endian:
//! ```text
//! 0   8  swapchain (u64)      16  2  width (u16)        20  1  codec (u8)
//! 8   4  image_index (u32)    18  2  height (u16)       21  3  zarezerwowane (zera)
//! 12  4  seq (u32)            20  ... patrz tabela      24  4  payload_len (u32)
//! ```
//! Dokładne offsety: swapchain@0, image_index@8, seq@12, width@16, height@18, format@20, codec@21, zero@22..24,
//! payload_len@24. Dane z sieci są niezaufane: `decode` waliduje wszystko i nigdy nie panikuje.

pub const HEADER_LEN: usize = 28;
pub const MAX_DIM: u16 = 16384;
pub const MAX_PAYLOAD: u32 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PixelFormat {
    Bgra8 = 1,
    Rgba8 = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    Raw = 0,
    Hevc = 1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoError {
    Truncated,
    BadFormat(u8),
    BadCodec(u8),
    /// Zerowy lub zbyt duży wymiar, albo niezgodność długości danych z wymiarami (dla `Raw`: w * h * 4).
    BadSize,
    TooLarge(u32),
    /// Zarezerwowane bajty nagłówka nie są zerami.
    Reserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub swapchain: u64,
    pub image_index: u32,
    pub seq: u32,
    pub width: u16,
    pub height: u16,
    pub format: PixelFormat,
    pub codec: Codec,
    pub payload_len: u32,
}

impl VideoHeader {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        todo!()
    }

    /// Odczytuje i waliduje nagłówek z początku `bytes` (nadmiarowe bajty za nagłówkiem są dozwolone).
    /// Kolejność kontroli: długość, format, kodek, zarezerwowane, wymiary, rozmiar danych.
    pub fn decode(bytes: &[u8]) -> Result<VideoHeader, VideoError> {
        todo!()
    }
}

/// Potwierdzenie klienta: ramka `seq` została zdekodowana i wyświetlona (4 B, little-endian).
pub fn encode_ack(seq: u32) -> [u8; 4] {
    todo!()
}

pub fn decode_ack(bytes: &[u8]) -> Result<u32, VideoError> {
    todo!()
}

/// Pakuje wiersze obrazu z odstępem `stride` (bajty na wiersz w pamięci źródłowej) w ciasny bufor w * h * 4 B.
/// Błąd `BadSize`, gdy wymiary są zerowe/za duże, `stride < w * 4` albo `pixels` jest za krótki
/// (potrzeba `stride * (h - 1) + w * 4` bajtów). Nie panikuje; rachunki w u64 z kontrolą przepełnienia.
pub fn pack_rows(pixels: &[u8], width: u16, height: u16, stride: usize) -> Result<Vec<u8>, VideoError> {
    todo!()
}

/// Zwraca dane surowej ramki (`Codec::Raw`) po sprawdzeniu, że `payload.len() == header.payload_len`
/// oraz `== w * h * 4`. Dla innych kodeków `BadCodec`.
pub fn raw_pixels<'a>(header: &VideoHeader, payload: &'a [u8]) -> Result<&'a [u8], VideoError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr() -> VideoHeader {
        VideoHeader { swapchain: 0x1122_3344_5566_7788, image_index: 2, seq: 0xAABB_CCDD, width: 1920, height: 1080, format: PixelFormat::Bgra8, codec: Codec::Raw, payload_len: 1920 * 1080 * 4 }
    }

    #[test]
    fn header_layout_is_fixed_little_endian() {
        let b = hdr().encode();
        assert_eq!(b.len(), 28);
        assert_eq!(&b[0..8], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&b[8..12], &2u32.to_le_bytes());
        assert_eq!(&b[12..16], &0xAABB_CCDDu32.to_le_bytes());
        assert_eq!(&b[16..18], &1920u16.to_le_bytes());
        assert_eq!(&b[18..20], &1080u16.to_le_bytes());
        assert_eq!((b[20], b[21]), (1, 0));
        assert_eq!(&b[22..24], &[0, 0]);
        assert_eq!(&b[24..28], &(1920u32 * 1080 * 4).to_le_bytes());
    }

    #[test]
    fn header_roundtrips_for_all_format_and_codec_combinations() {
        for f in [PixelFormat::Bgra8, PixelFormat::Rgba8] {
            for c in [Codec::Raw, Codec::Hevc] {
                let mut h = hdr();
                h.format = f;
                h.codec = c;
                if c == Codec::Hevc {
                    h.payload_len = 12345;
                }
                assert_eq!(VideoHeader::decode(&h.encode()), Ok(h));
            }
        }
    }

    #[test]
    fn decode_ignores_trailing_bytes_and_rejects_short_input() {
        let mut v = hdr().encode().to_vec();
        v.extend([9, 9, 9]);
        assert_eq!(VideoHeader::decode(&v), Ok(hdr()));
        for cut in 0..HEADER_LEN {
            assert_eq!(VideoHeader::decode(&v[..cut]), Err(VideoError::Truncated), "{cut}");
        }
    }

    #[test]
    fn unknown_format_codec_and_reserved_bytes_are_rejected_in_documented_order() {
        let base = hdr().encode();
        let mut b = base;
        b[20] = 0;
        assert_eq!(VideoHeader::decode(&b), Err(VideoError::BadFormat(0)));
        b[20] = 9;
        assert_eq!(VideoHeader::decode(&b), Err(VideoError::BadFormat(9)));
        let mut b = base;
        b[21] = 7;
        assert_eq!(VideoHeader::decode(&b), Err(VideoError::BadCodec(7)));
        let mut b = base;
        b[22] = 1;
        assert_eq!(VideoHeader::decode(&b), Err(VideoError::Reserved));
        let mut b = base;
        b[23] = 1;
        assert_eq!(VideoHeader::decode(&b), Err(VideoError::Reserved));
        // Format sprawdzany przed kodekiem.
        let mut b = base;
        b[20] = 9;
        b[21] = 7;
        assert_eq!(VideoHeader::decode(&b), Err(VideoError::BadFormat(9)));
    }

    #[test]
    fn dimensions_and_raw_payload_size_are_validated() {
        for (w, h) in [(0u16, 100u16), (100, 0), (MAX_DIM + 1, 100), (100, MAX_DIM + 1), (u16::MAX, u16::MAX)] {
            let mut x = hdr();
            x.width = w;
            x.height = h;
            x.payload_len = 0;
            assert_eq!(VideoHeader::decode(&x.encode()), Err(VideoError::BadSize), "{w}x{h}");
        }
        let mut x = hdr();
        x.payload_len -= 1;
        assert_eq!(VideoHeader::decode(&x.encode()), Err(VideoError::BadSize), "dla Raw długość musi być równa w*h*4");
        let mut x = hdr();
        x.width = MAX_DIM;
        x.height = MAX_DIM;
        x.payload_len = u32::MAX;
        assert_eq!(VideoHeader::decode(&x.encode()), Err(VideoError::TooLarge(u32::MAX)), "limit MAX_PAYLOAD przed porównaniem z wymiarami");
        let mut x = hdr();
        x.codec = Codec::Hevc;
        x.payload_len = MAX_PAYLOAD + 1;
        assert_eq!(VideoHeader::decode(&x.encode()), Err(VideoError::TooLarge(MAX_PAYLOAD + 1)));
        x.payload_len = MAX_PAYLOAD;
        assert!(VideoHeader::decode(&x.encode()).is_ok());
        x.payload_len = 0;
        assert!(VideoHeader::decode(&x.encode()).is_ok(), "pusty ładunek HEVC jest dozwolony przy poprawnych wymiarach");
    }

    #[test]
    fn ack_roundtrips_and_rejects_wrong_length() {
        for s in [0u32, 1, 0xDEAD_BEEF, u32::MAX] {
            assert_eq!(decode_ack(&encode_ack(s)), Ok(s));
        }
        assert_eq!(encode_ack(1), [1, 0, 0, 0]);
        assert_eq!(decode_ack(&[1, 2, 3]), Err(VideoError::Truncated));
        assert_eq!(decode_ack(&[]), Err(VideoError::Truncated));
        assert_eq!(decode_ack(&[1, 2, 3, 4, 5]), Err(VideoError::BadSize), "nadmiarowe bajty potwierdzenia");
    }

    #[test]
    fn pack_rows_strips_stride_padding() {
        // 2x3 piksele, stride 12 B (8 B danych + 4 B wypełnienia).
        let mut src = vec![0u8; 12 * 3];
        for r in 0..3 {
            for i in 0..8 {
                src[r * 12 + i] = (r * 10 + i) as u8 + 1;
            }
            for i in 8..12 {
                src[r * 12 + i] = 0xEE;
            }
        }
        let out = pack_rows(&src, 2, 3, 12).unwrap();
        assert_eq!(out.len(), 24);
        assert!(!out.contains(&0xEE));
        assert_eq!(&out[8..16], &src[12..20]);
        // Ostatni wiersz nie wymaga wypełnienia: wystarcza stride*(h-1)+w*4.
        let short = &src[..12 * 2 + 8];
        assert_eq!(pack_rows(short, 2, 3, 12).unwrap(), out);
        // Ciasny stride = kopia.
        let tight: Vec<u8> = (0..24).collect();
        assert_eq!(pack_rows(&tight, 2, 3, 8).unwrap(), tight);
    }

    #[test]
    fn pack_rows_rejects_bad_geometry_without_panicking() {
        let px = vec![0u8; 64];
        assert_eq!(pack_rows(&px, 0, 4, 16), Err(VideoError::BadSize));
        assert_eq!(pack_rows(&px, 4, 0, 16), Err(VideoError::BadSize));
        assert_eq!(pack_rows(&px, MAX_DIM + 1, 1, usize::MAX), Err(VideoError::BadSize));
        assert_eq!(pack_rows(&px, 4, 4, 15), Err(VideoError::BadSize), "stride mniejszy niż w*4");
        assert_eq!(pack_rows(&px, 4, 4, 16 + 1), Err(VideoError::BadSize), "za krótki bufor dla stride 17");
        assert_eq!(pack_rows(&px[..63], 4, 4, 16), Err(VideoError::BadSize));
        assert_eq!(pack_rows(&px, 4, 4, usize::MAX), Err(VideoError::BadSize), "przepełnienie stride * (h-1)");
        assert!(pack_rows(&px, 4, 4, 16).is_ok());
    }

    #[test]
    fn raw_pixels_checks_codec_and_both_lengths() {
        let mut h = hdr();
        h.width = 4;
        h.height = 2;
        h.payload_len = 32;
        let payload = vec![7u8; 32];
        assert_eq!(raw_pixels(&h, &payload), Ok(&payload[..]));
        assert_eq!(raw_pixels(&h, &payload[..31]), Err(VideoError::BadSize));
        let mut long = payload.clone();
        long.push(0);
        assert_eq!(raw_pixels(&h, &long), Err(VideoError::BadSize));
        let mut inconsistent = h;
        inconsistent.payload_len = 33;
        assert_eq!(raw_pixels(&inconsistent, &long), Err(VideoError::BadSize), "payload_len różne od w*h*4");
        let mut hevc = h;
        hevc.codec = Codec::Hevc;
        assert_eq!(raw_pixels(&hevc, &payload), Err(VideoError::BadCodec(1)));
    }

    #[test]
    fn random_headers_never_panic() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let len = (next() % 40) as usize;
            let b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = VideoHeader::decode(&b);
            let _ = decode_ack(&b);
        }
    }
}
