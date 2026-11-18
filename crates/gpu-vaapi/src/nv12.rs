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
        let pixels = row.as_chunks_mut::<4>().0;

        // Indeks pierwszej próbki UV dla piksela x=0 to (ox/2)*2.
        let uv_base = (ox / 2) * 2;
        // Liczba pełnych par (x, x+1) które dzielą jedną próbkę UV.
        // Parowanie: piksele (0,1), (2,3), ... względem ox. Gdy ox jest nieparzyste,
        // piksel 0 ma własną próbkę UV (parę tworzy z pikselem poza zakresem), więc
        // obsługujemy go osobno, a resztę parujemy od x=1.
        let start = if ox % 2 == 1 { 1 } else { 0 };

        // Piksel brzegowy (gdy ox nieparzyste): x=0 używa próbki UV o indeksie uv_base.
        if start == 1 && w > 0 {
            let c = 298 * (yrow[0] as i32 - 16);
            let u = uvrow[uv_base] as i32 - 128;
            let v = uvrow[uv_base + 1] as i32 - 128;
            let rr = (c + 409 * v + 128) >> 8;
            let gg = (c - 100 * u - 208 * v + 128) >> 8;
            let bb = (c + 516 * u + 128) >> 8;
            let px = &mut pixels[0];
            px[0] = bb.clamp(0, 255) as u8;
            px[1] = gg.clamp(0, 255) as u8;
            px[2] = rr.clamp(0, 255) as u8;
        }

        // Pary pikseli: każda para dzieli jedną próbkę UV.
        let pair_count = (w - start) / 2;
        let y_pairs = &yrow[start..start + pair_count * 2];
        let px_pairs = &mut pixels[start..start + pair_count * 2];
        // Próbki UV dla par: indeksy uv_base + (start + 2*k) ... ale start jest 0 lub 1.
        // Dla pary k (piksele start+2k, start+2k+1), próbka UV to ((ox + start + 2k)/2)*2.
        // Gdy start=0: (ox/2 + k)*2 = uv_base + 2k.
        // Gdy start=1: ((ox+1)/2 + k)*2 = uv_base + 2 + 2k (bo ox nieparzyste, (ox+1)/2 = ox/2 + 1).
        let uv_pair_off = if start == 1 { 2 } else { 0 };
        let uv_pairs = &uvrow[uv_base + uv_pair_off..];

        for (k, (yp, px)) in y_pairs.chunks_exact(2).zip(px_pairs.chunks_exact_mut(2)).enumerate() {
            let u = uv_pairs[2 * k] as i32 - 128;
            let v = uv_pairs[2 * k + 1] as i32 - 128;
            let du = 516 * u;
            let dv = 409 * v;
            let gu = 100 * u;
            let gv = 208 * v;

            let c0 = 298 * (yp[0] as i32 - 16);
            let c1 = 298 * (yp[1] as i32 - 16);

            let r0 = (c0 + dv + 128) >> 8;
            let g0 = (c0 - gu - gv + 128) >> 8;
            let b0 = (c0 + du + 128) >> 8;
            let r1 = (c1 + dv + 128) >> 8;
            let g1 = (c1 - gu - gv + 128) >> 8;
            let b1 = (c1 + du + 128) >> 8;

            px[0][0] = b0.clamp(0, 255) as u8;
            px[0][1] = g0.clamp(0, 255) as u8;
            px[0][2] = r0.clamp(0, 255) as u8;
            px[1][0] = b1.clamp(0, 255) as u8;
            px[1][1] = g1.clamp(0, 255) as u8;
            px[1][2] = r1.clamp(0, 255) as u8;
        }

        // Ostatni piksel, gdy (w - start) jest nieparzyste.
        if (w - start) % 2 == 1 {
            let x = w - 1;
            let c = 298 * (yrow[x] as i32 - 16);
            let uvx = ((ox + x) / 2) * 2;
            let u = uvrow[uvx] as i32 - 128;
            let v = uvrow[uvx + 1] as i32 - 128;
            let rr = (c + 409 * v + 128) >> 8;
            let gg = (c - 100 * u - 208 * v + 128) >> 8;
            let bb = (c + 516 * u + 128) >> 8;
            let px = &mut pixels[x];
            px[0] = bb.clamp(0, 255) as u8;
            px[1] = gg.clamp(0, 255) as u8;
            px[2] = rr.clamp(0, 255) as u8;
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
