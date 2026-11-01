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
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (ox + x, oy + y);
            let yy = data[y_off + sy * y_pitch + sx] as i32;
            let u = data[uv_off + (sy / 2) * uv_pitch + (sx / 2) * 2] as i32 - 128;
            let v = data[uv_off + (sy / 2) * uv_pitch + (sx / 2) * 2 + 1] as i32 - 128;
            let c = 298 * (yy - 16);
            let r = (c + 409 * v + 128) >> 8;
            let g = (c - 100 * u - 208 * v + 128) >> 8;
            let b = (c + 516 * u + 128) >> 8;
            let o = (y * w + x) * 4;
            out[o] = b.clamp(0, 255) as u8;
            out[o + 1] = g.clamp(0, 255) as u8;
            out[o + 2] = r.clamp(0, 255) as u8;
        }
    }
    Ok(out)
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
