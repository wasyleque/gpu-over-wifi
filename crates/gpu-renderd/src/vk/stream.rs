//! Kopiowanie z pamięci odwzorowanej, którą procesor czyta powoli (write-combined, przez PCIe): zwykłe odczyty takiej pamięci
//! idą 16 bajtów na obieg magistrali, strumieniowe (`MOVNTDQA`, SSE4.1) zbierają całe linie, co bywa wielokrotnie szybsze.
//! Skróty stron liczy się potem z kopii w zwykłej pamięci.

/// Kopiuje `len` bajtów z `src` do `dst` (rozłączne obszary), pamięć źródłową czytając strumieniowo, gdy procesor to umie.
///
/// # Safety
/// `src` musi być czytelne, a `dst` zapisywalne na `len` bajtów; obszary się nie nakładają.
pub unsafe fn stream_copy(dst: *mut u8, src: *const u8, len: usize) {
    #[cfg(target_arch = "x86_64")]
    {
        if len >= 64 && is_x86_feature_detected!("sse4.1") {
            return copy_sse41(dst, src, len);
        }
    }
    std::ptr::copy_nonoverlapping(src, dst, len);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn copy_sse41(dst: *mut u8, src: *const u8, len: usize) {
    use std::arch::x86_64::{__m128i, _mm_storeu_si128, _mm_stream_load_si128};
    // Początek do wyrównania źródła do 16 bajtów kopiujemy zwykle, środek strumieniowo (po 64 B), resztę zwykle.
    let head = (16 - (src as usize & 15)) & 15;
    let head = head.min(len);
    std::ptr::copy_nonoverlapping(src, dst, head);
    let (mut s, mut d, mut left) = (src.add(head), dst.add(head), len - head);
    while left >= 64 {
        let a = _mm_stream_load_si128(s as *mut __m128i);
        let b = _mm_stream_load_si128(s.add(16) as *mut __m128i);
        let c = _mm_stream_load_si128(s.add(32) as *mut __m128i);
        let e = _mm_stream_load_si128(s.add(48) as *mut __m128i);
        _mm_storeu_si128(d as *mut __m128i, a);
        _mm_storeu_si128(d.add(16) as *mut __m128i, b);
        _mm_storeu_si128(d.add(32) as *mut __m128i, c);
        _mm_storeu_si128(d.add(48) as *mut __m128i, e);
        s = s.add(64);
        d = d.add(64);
        left -= 64;
    }
    std::ptr::copy_nonoverlapping(s, d, left);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_exactly_for_every_alignment_and_length() {
        let src: Vec<u8> = (0..4096u32).map(|i| (i * 31 + 7) as u8).collect();
        for off in 0..20usize {
            for len in [0usize, 1, 15, 16, 17, 63, 64, 65, 200, 1000, 4000] {
                let len = len.min(src.len() - off);
                let mut dst = vec![0xEEu8; len + 8];
                unsafe { stream_copy(dst.as_mut_ptr(), src.as_ptr().add(off), len) };
                assert_eq!(&dst[..len], &src[off..off + len], "off {off} len {len}");
                assert!(dst[len..].iter().all(|b| *b == 0xEE), "zapis poza zakresem: off {off} len {len}");
            }
        }
    }
}
