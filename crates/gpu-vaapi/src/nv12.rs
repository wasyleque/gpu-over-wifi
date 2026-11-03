//! Konwersja NV12 (BT.601, zakres ograniczony) do BGRA na CPU.

/// `data` zawiera płaszczyznę Y (`y_pitch`, od `y_off`) i przeplecione UV (`uv_pitch`, od `uv_off`). `origin` to lewy górny róg
/// przycięcia, `size` to wymiary wyjścia. Błąd, gdy płaszczyzny nie mieszczą się w `data`.
pub fn nv12_to_bgra(data: &[u8], y_pitch: usize, y_off: usize, uv_pitch: usize, uv_off: usize, origin: (usize, usize), size: (usize, usize)) -> Result<Vec<u8>, String> {
    let (ox, oy) = origin;
    let (w, h) = size;
    let y_end = y_off.checked_add((oy + h).checked_mul(y_pitch).ok_or("przepełnienie")?).ok_or("przepełnienie")?;
    let uv_end = uv_off.checked_add(((oy + h).div_ceil(2)).checked_mul(uv_pitch).ok_or("przepełnienie")?).ok_or("przepełnienie")?;
    if y_pitch < ox + w || uv_pitch < ((ox + w).div_ceil(2)) * 2 || y_end > data.len() || uv_end > data.len() {
        return Err("płaszczyzny NV12 poza buforem".into());
    }
    let mut out = vec![255u8; w * h * 4];
    if w == 0 || h == 0 {
        return Ok(out);
    }
    // Pasma wierszy konwertowane równolegle (tyle wątków, ile rdzeni, ale nie za cienkie pasma).
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8).min((h / 32).max(1));
    let rows_per = h.div_ceil(threads);
    std::thread::scope(|sc| {
        for (band, chunk) in out.chunks_mut(rows_per * w * 4).enumerate() {
            let y0 = band * rows_per;
            sc.spawn(move || convert_band(data, y_pitch, y_off, uv_pitch, uv_off, origin, w, y0, chunk));
        }
    });
    Ok(out)
}

/// Konwertuje wiersze `y0..` (tyle, ile mieści `out`) wyjścia; wszystkie indeksy zostały sprawdzone przez wołającego.
#[allow(clippy::too_many_arguments)]
fn convert_band(data: &[u8], y_pitch: usize, y_off: usize, uv_pitch: usize, uv_off: usize, origin: (usize, usize), w: usize, y0: usize, out: &mut [u8]) {
    let (ox, oy) = origin;
    for (r, row) in out.chunks_exact_mut(w * 4).enumerate() {
        let sy = oy + y0 + r;
        let yrow = &data[y_off + sy * y_pitch + ox..][..w];
        let uvrow = &data[uv_off + (sy / 2) * uv_pitch..][..uv_pitch];
        let (pixels, _) = row.as_chunks_mut::<4>();
        for (x, px) in pixels.iter_mut().enumerate() {
            let c = 298 * (yrow[x] as i32 - 16);
            let uvx = ((ox + x) / 2) * 2;
            let (u, v) = (uvrow[uvx] as i32 - 128, uvrow[uvx + 1] as i32 - 128);
            let r = (c + 409 * v + 128) >> 8;
            let g = (c - 100 * u - 208 * v + 128) >> 8;
            let b = (c + 516 * u + 128) >> 8;
            px[0] = b.clamp(0, 255) as u8;
            px[1] = g.clamp(0, 255) as u8;
            px[2] = r.clamp(0, 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_white_and_gray_map_correctly() {
        // 2x2: Y = 16 (czarny), 235 (biały), 126 (szary); UV neutralne.
        let mut d = vec![0u8; 4 + 2];
        d[0..4].copy_from_slice(&[16, 235, 126, 126]);
        d[4] = 128;
        d[5] = 128;
        let o = nv12_to_bgra(&d, 2, 0, 2, 4, (0, 0), (2, 2)).unwrap();
        assert_eq!(&o[0..4], &[0, 0, 0, 255]);
        assert_eq!(&o[4..8], &[255, 255, 255, 255]);
        assert!((o[8] as i32 - 128).abs() <= 2 && o[8] == o[9] && o[9] == o[10]);
    }

    /// Prosta, jednowątkowa wersja wzorcowa (ten sam wzór co produkcyjna).
    fn reference(data: &[u8], yp: usize, yo: usize, uvp: usize, uvo: usize, origin: (usize, usize), size: (usize, usize)) -> Vec<u8> {
        let (ox, oy) = origin;
        let (w, h) = size;
        let mut out = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (ox + x, oy + y);
                let yy = data[yo + sy * yp + sx] as i32;
                let u = data[uvo + (sy / 2) * uvp + (sx / 2) * 2] as i32 - 128;
                let v = data[uvo + (sy / 2) * uvp + (sx / 2) * 2 + 1] as i32 - 128;
                let c = 298 * (yy - 16);
                let o = (y * w + x) * 4;
                out[o] = ((c + 516 * u + 128) >> 8).clamp(0, 255) as u8;
                out[o + 1] = ((c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
                out[o + 2] = ((c + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            }
        }
        out
    }

    #[test]
    fn parallel_conversion_matches_the_reference_for_odd_crops_and_many_sizes() {
        // Pseudolosowe dane; różne rozmiary, w tym nieparzyste przycięcie i wysokości dzielące się różnie przez liczbę wątków.
        let mut seed = 12345u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        for (w, h, ox, oy) in [(64usize, 64usize, 0usize, 0usize), (130, 97, 3, 5), (200, 33, 1, 1), (17, 300, 0, 3), (1920, 100, 8, 0)] {
            let (yp, uvp) = (ox + w + 4, (ox + w).div_ceil(2) * 2 + 2);
            let (yh, uvh) = (oy + h + 2, (oy + h).div_ceil(2) + 1);
            let mut data: Vec<u8> = (0..yp * yh + uvp * uvh).map(|_| next()).collect();
            data.extend_from_slice(&[0; 8]);
            let got = nv12_to_bgra(&data, yp, 0, uvp, yp * yh, (ox, oy), (w, h)).unwrap();
            assert_eq!(got, reference(&data, yp, 0, uvp, yp * yh, (ox, oy), (w, h)), "{w}x{h} @ {ox},{oy}");
        }
    }

    #[test]
    fn crop_origin_and_out_of_bounds_planes() {
        let d = vec![100u8; 16 * 16 + 16 * 8];
        let o = nv12_to_bgra(&d, 16, 0, 16, 256, (4, 2), (8, 6)).unwrap();
        assert_eq!(o.len(), 8 * 6 * 4);
        assert!(nv12_to_bgra(&d, 16, 0, 16, 256, (4, 2), (8, 20)).is_err());
        assert!(nv12_to_bgra(&d, 16, 0, 16, 256, (12, 0), (8, 4)).is_err());
        assert!(nv12_to_bgra(&d, usize::MAX, 0, 16, 256, (0, 0), (8, 4)).is_err());
    }
}
