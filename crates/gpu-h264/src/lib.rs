//! Minimalny parser strumienia H.264 (Annex B) dla naszego własnego kodera: dzieli strumień na jednostki NAL i czyta SPS, PPS
//! oraz nagłówek slice'a, czyli dane potrzebne dekoderowi sprzętowemu (VA-API). Obsługuje WYŁĄCZNIE profil, który sami
//! konfigurujemy w NVENC: Main/High 4:2:0 8-bit, tylko ramki (`frame_mbs_only`), jeden slice na ramkę, slice'y I i P, bez B-ramek,
//! bez macierzy skalujących, bez grup slice'ów, bez ważonej predykcji, bez modyfikacji list odniesienia i bez MMCO.
//! Wszystko inne to `Unsupported`, a nie niepoprawny wynik. Dane z sieci są niezaufane: żadna funkcja nie panikuje.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264Error {
    /// Za mało bitów/bajtów.
    Truncated,
    /// Poprawne wg standardu, ale poza naszym profilem.
    Unsupported(&'static str),
    /// Niezgodne ze standardem (np. zbyt długi kod Golomba, zły bit zabroniony).
    Invalid(&'static str),
}

/// Czytnik bitów (MSB pierwszy) po RBSP (bez bajtów zapobiegania emulacji).
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// Liczba przeczytanych bitów.
    pub fn pos_bits(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    pub fn read_bit(&mut self) -> Result<u32, H264Error> {
        self.read_bits(1)
    }

    /// Czyta `n` bitów (0..=32) jako liczbę bez znaku.
    pub fn read_bits(&mut self, n: u32) -> Result<u32, H264Error> {
        if n > 32 {
            return Err(H264Error::Invalid("read_bits > 32"));
        }
        if n == 0 {
            return Ok(0);
        }
        let need = n as usize;
        if self.bits_left() < need {
            return Err(H264Error::Truncated);
        }
        let mut v: u32 = 0;
        for _ in 0..need {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | bit as u32;
            self.pos += 1;
        }
        Ok(v)
    }

    /// Kod Golomba bez znaku `ue(v)`. Więcej niż 31 zer wiodących to `Invalid`.
    pub fn read_ue(&mut self) -> Result<u32, H264Error> {
        let mut zeros = 0u32;
        loop {
            if self.bits_left() == 0 {
                return Err(H264Error::Truncated);
            }
            let b = self.read_bit()?;
            if b == 1 {
                break;
            }
            zeros += 1;
            if zeros >= 32 {
                return Err(H264Error::Invalid("ue too long"));
            }
        }
        let suffix = self.read_bits(zeros)?;
        let val = (1u64 << zeros) - 1 + suffix as u64;
        if val > u32::MAX as u64 {
            return Err(H264Error::Invalid("ue too long"));
        }
        Ok(val as u32)
    }

    /// Kod Golomba ze znakiem `se(v)`: 1 -> +1, 2 -> -1, 3 -> +2, 4 -> -2, ...
    pub fn read_se(&mut self) -> Result<i32, H264Error> {
        let k = self.read_ue()?;
        if k % 2 == 1 {
            Ok(k.div_ceil(2) as i32)
        } else {
            Ok(-((k / 2) as i32))
        }
    }
}

/// Dzieli strumień Annex B na jednostki NAL (bez kodów startowych `00 00 01` / `00 00 00 01`; kończące zera `trailing_zero_8bits`
/// są obcinane). Dane przed pierwszym kodem startowym są ignorowane. Puste NAL-e są pomijane.
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (idx, &s) in starts.iter().enumerate() {
        let end = if idx + 1 < starts.len() {
            starts[idx + 1] - 3
        } else {
            data.len()
        };
        let mut e = end;
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

/// Nagłówek NAL (1 bajt): `(nal_ref_idc, nal_unit_type)`. Bit zabroniony musi być 0; pusty NAL to `Truncated`.
pub fn nal_header(nal: &[u8]) -> Result<(u8, u8), H264Error> {
    let b = *nal.first().ok_or(H264Error::Truncated)?;
    if b & 0x80 != 0 {
        return Err(H264Error::Invalid("forbidden_zero_bit"));
    }
    Ok(((b >> 5) & 3, b & 0x1f))
}

/// RBSP z jednostki NAL: pomija bajt nagłówka i usuwa bajty zapobiegania emulacji (`00 00 03` -> `00 00`).
pub fn unescape_rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut zeros = 0u32;
    for &b in nal.iter().skip(1) {
        if zeros == 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    out
}

/// Przelicza pozycję bitową w RBSP (po nagłówku) na pozycję bitową w oryginalnym NAL liczoną od bajtu nagłówka,
/// uwzględniając bajt nagłówka (+8) i bajty zapobiegania emulacji, które leżą przed tą pozycją.
pub fn escaped_bit_offset(nal: &[u8], rbsp_bit: usize) -> usize {
    let target_byte = rbsp_bit / 8;
    let mut removed = 0usize;
    let mut zeros = 0u32;
    let mut rbsp_idx = 0usize;
    for &b in nal.iter().skip(1) {
        if zeros == 2 && b == 3 {
            // Bajt zapobiegania emulacji stoi tuż przed bajtem RBSP o indeksie `rbsp_idx`.
            if rbsp_idx <= target_byte {
                removed += 1;
            }
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp_idx += 1;
    }
    8usize.saturating_add(rbsp_bit).saturating_add(removed.saturating_mul(8))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sps {
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub sps_id: u32,
    pub chroma_format_idc: u32,
    /// `log2_max_frame_num_minus4 + 4`.
    pub log2_max_frame_num: u32,
    pub poc_type: u32,
    /// `log2_max_pic_order_cnt_lsb_minus4 + 4` (tylko `poc_type == 0`, inaczej 0).
    pub log2_max_poc_lsb: u32,
    pub max_num_ref_frames: u32,
    /// Szerokość w makroblokach (`pic_width_in_mbs_minus1 + 1`).
    pub width_mbs: u32,
    /// Wysokość w makroblokach (`pic_height_in_map_units_minus1 + 1`; przy `frame_mbs_only` równa wysokości ramki).
    pub height_mbs: u32,
    pub direct_8x8_inference: bool,
    /// Przycięcie w jednostkach próbek: (lewo, prawo, góra, dół).
    pub crop: (u32, u32, u32, u32),
    pub vui_present: bool,
}

impl Sps {
    /// Wymiary obrazu po przycięciu (4:2:0: jednostka przycięcia 2 próbki w poziomie i 2 w pionie).
    pub fn size(&self) -> (u32, u32) {
        let w = self.width_mbs.saturating_mul(16).saturating_sub(self.crop.0).saturating_sub(self.crop.1);
        let h = self.height_mbs.saturating_mul(16).saturating_sub(self.crop.2).saturating_sub(self.crop.3);
        (w, h)
    }
}

/// Czyta SPS z RBSP (bez nagłówka NAL). Pola VUI po `vui_parameters_present_flag` są pomijane (nie są potrzebne).
/// `Unsupported`: `frame_mbs_only == 0`, `poc_type == 1`, `chroma_format_idc != 1`, głębia bitowa != 8, macierze skalujące,
/// `qpprime_y_zero_transform_bypass`, profile poza Main(77)/High(100)/Constrained Baseline(66 z constraint_set1).
pub fn parse_sps(rbsp: &[u8]) -> Result<Sps, H264Error> {
    let mut r = BitReader::new(rbsp);
    let profile_idc = r.read_bits(8)? as u8;
    let constraint_flags = r.read_bits(8)? as u8;
    let level_idc = r.read_bits(8)? as u8;
    let sps_id = r.read_ue()?;

    let high_profiles = [100u8, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
    let mut chroma_format_idc = 1u32;
    let mut bit_depth_luma = 8u32;
    let mut bit_depth_chroma = 8u32;

    if high_profiles.contains(&profile_idc) {
        chroma_format_idc = r.read_ue()?;
        if chroma_format_idc == 3 {
            let _ = r.read_bit()?;
        }
        bit_depth_luma = r.read_ue()? + 8;
        bit_depth_chroma = r.read_ue()? + 8;
        let qpprime = r.read_bit()?;
        if qpprime == 1 {
            return Err(H264Error::Unsupported("qpprime_y_zero_transform_bypass"));
        }
        let seq_scaling = r.read_bit()?;
        if seq_scaling == 1 {
            return Err(H264Error::Unsupported("seq_scaling_matrix_present"));
        }
    }

    if profile_idc != 77 && profile_idc != 100 && !(profile_idc == 66 && constraint_flags & 0x02 != 0) {
        return Err(H264Error::Unsupported("profile"));
    }

    if chroma_format_idc != 1 || bit_depth_luma != 8 || bit_depth_chroma != 8 {
        return Err(H264Error::Unsupported("chroma/bit_depth"));
    }

    let log2_max_frame_num = r.read_ue()? + 4;
    let poc_type = r.read_ue()?;
    if poc_type == 1 {
        return Err(H264Error::Unsupported("pic_order_cnt_type 1"));
    }
    if poc_type > 2 {
        return Err(H264Error::Invalid("pic_order_cnt_type"));
    }
    let log2_max_poc_lsb = if poc_type == 0 { r.read_ue()? + 4 } else { 0 };
    let max_num_ref_frames = r.read_ue()?;
    let _gaps = r.read_bit()?;
    let width_mbs = r.read_ue()? + 1;
    let height_mbs = r.read_ue()? + 1;
    if width_mbs == 0 || width_mbs > 1024 || height_mbs == 0 || height_mbs > 1024 {
        return Err(H264Error::Invalid("absurd size"));
    }
    let frame_mbs_only = r.read_bit()?;
    if frame_mbs_only == 0 {
        return Err(H264Error::Unsupported("frame_mbs_only_flag 0"));
    }
    let direct_8x8 = r.read_bit()?;
    let crop_flag = r.read_bit()?;
    let crop = if crop_flag == 1 {
        let l = r.read_ue()? * 2;
        let rt = r.read_ue()? * 2;
        let t = r.read_ue()? * 2;
        let b = r.read_ue()? * 2;
        (l, rt, t, b)
    } else {
        (0, 0, 0, 0)
    };
    let vui_present = r.read_bit()?;

    Ok(Sps {
        profile_idc,
        constraint_flags,
        level_idc,
        sps_id,
        chroma_format_idc,
        log2_max_frame_num,
        poc_type,
        log2_max_poc_lsb,
        max_num_ref_frames,
        width_mbs,
        height_mbs,
        direct_8x8_inference: direct_8x8 == 1,
        crop,
        vui_present: vui_present == 1,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pps {
    pub pps_id: u32,
    pub sps_id: u32,
    /// `true` = CABAC, `false` = CAVLC.
    pub cabac: bool,
    pub bottom_field_pic_order_in_frame_present: bool,
    /// `num_ref_idx_l0_default_active_minus1 + 1`.
    pub num_ref_idx_l0_default_active: u32,
    pub weighted_pred: bool,
    /// `26 + pic_init_qp_minus26`.
    pub pic_init_qp: i32,
    pub chroma_qp_index_offset: i32,
    pub deblocking_filter_control_present: bool,
    pub constrained_intra_pred: bool,
    pub redundant_pic_cnt_present: bool,
    /// `transform_8x8_mode_flag` z rozszerzenia PPS (domyślnie `false`, gdy rozszerzenia brak).
    pub transform_8x8_mode: bool,
}

/// Czyta PPS z RBSP. `Unsupported`: grupy slice'ów (`num_slice_groups_minus1 > 0`), `weighted_bipred_idc != 0`,
/// macierze skalujące w rozszerzeniu PPS.
pub fn parse_pps(rbsp: &[u8]) -> Result<Pps, H264Error> {
    let mut r = BitReader::new(rbsp);
    let pps_id = r.read_ue()?;
    let sps_id = r.read_ue()?;
    let cabac = r.read_bit()?;
    let bottom_field = r.read_bit()?;
    let num_slice_groups = r.read_ue()?;
    if num_slice_groups > 0 {
        return Err(H264Error::Unsupported("num_slice_groups"));
    }
    let num_ref_l0 = r.read_ue()? + 1;
    let _num_ref_l1 = r.read_ue()? + 1;
    let weighted_pred = r.read_bit()?;
    let weighted_bipred = r.read_bits(2)?;
    if weighted_bipred != 0 {
        return Err(H264Error::Unsupported("weighted_bipred_idc"));
    }
    let pic_init_qp = r.read_se()? + 26;
    let _pic_init_qs = r.read_se()? + 26;
    let chroma_qp = r.read_se()?;
    let deblocking = r.read_bit()?;
    let constrained = r.read_bit()?;
    let redundant = r.read_bit()?;

    // Check if there's more data before trailing bits
    let mut transform_8x8 = false;
    let total_bits = rbsp.len() * 8;
    let pos = r.pos_bits();
    if pos < total_bits {
        // Find the last 1 bit in the RBSP
        let mut last_one = None;
        for i in (0..total_bits).rev() {
            if i < rbsp.len() * 8 {
                let byte = rbsp[i / 8];
                let bit = (byte >> (7 - (i % 8))) & 1;
                if bit == 1 {
                    last_one = Some(i);
                    break;
                }
            }
        }
        if let Some(lo) = last_one {
            if pos < lo {
                transform_8x8 = r.read_bit()? == 1;
                let scaling = r.read_bit()?;
                if scaling == 1 {
                    return Err(H264Error::Unsupported("pic_scaling_matrix_present"));
                }
                let _second_chroma = r.read_se()?;
            }
        }
    }

    Ok(Pps {
        pps_id,
        sps_id,
        cabac: cabac == 1,
        bottom_field_pic_order_in_frame_present: bottom_field == 1,
        num_ref_idx_l0_default_active: num_ref_l0,
        weighted_pred: weighted_pred == 1,
        pic_init_qp,
        chroma_qp_index_offset: chroma_qp,
        deblocking_filter_control_present: deblocking == 1,
        constrained_intra_pred: constrained == 1,
        redundant_pic_cnt_present: redundant == 1,
        transform_8x8_mode: transform_8x8,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    P,
    I,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceHeader {
    pub nal_unit_type: u8,
    pub nal_ref_idc: u8,
    pub first_mb_in_slice: u32,
    pub slice_type: SliceType,
    pub pps_id: u32,
    pub frame_num: u32,
    pub idr_pic_id: Option<u32>,
    pub poc_lsb: Option<u32>,
    pub delta_poc_bottom: Option<i32>,
    pub redundant_pic_cnt: Option<u32>,
    /// `Some(n)` tylko gdy `num_ref_idx_active_override_flag == 1` (n = `num_ref_idx_l0_active_minus1 + 1`).
    pub num_ref_idx_l0_active_override: Option<u32>,
    pub cabac_init_idc: Option<u32>,
    pub slice_qp_delta: i32,
    pub disable_deblocking_filter_idc: u32,
    pub slice_alpha_c0_offset_div2: i32,
    pub slice_beta_offset_div2: i32,
    pub no_output_of_prior_pics: bool,
    pub long_term_reference: bool,
    /// Liczba bitów RBSP zajętych przez nagłówek slice'a (dane slice'a zaczynają się zaraz po nich).
    pub header_bits: usize,
}

/// Czyta nagłówek slice'a (RBSP bez nagłówka NAL) w kontekście `sps` i `pps`. `nal_unit_type` 5 = IDR, 1 = nie-IDR.
/// `Unsupported`: slice B/SP/SI, modyfikacja list odniesienia, ważona predykcja, `adaptive_ref_pic_marking_mode_flag == 1`,
/// `poc_type == 1`, `separate_colour_plane`. `Invalid`: `nal_unit_type` spoza {1, 5}, `slice_type` > 9, `pps_id` niezgodny z `pps`.
pub fn parse_slice_header(rbsp: &[u8], nal_unit_type: u8, nal_ref_idc: u8, sps: &Sps, pps: &Pps) -> Result<SliceHeader, H264Error> {
    if nal_unit_type != 1 && nal_unit_type != 5 {
        return Err(H264Error::Invalid("nal_unit_type"));
    }
    let mut r = BitReader::new(rbsp);
    let first_mb = r.read_ue()?;
    let st = r.read_ue()?;
    if st > 9 {
        return Err(H264Error::Invalid("slice_type"));
    }
    let st_mod = st % 5;
    let slice_type = match st_mod {
        0 => SliceType::P,
        2 => SliceType::I,
        _ => return Err(H264Error::Unsupported("slice_type")),
    };
    let pps_id = r.read_ue()?;
    if pps_id != pps.pps_id {
        return Err(H264Error::Invalid("pps_id"));
    }
    let frame_num = r.read_bits(sps.log2_max_frame_num)?;
    let is_idr = nal_unit_type == 5;
    let idr_pic_id = if is_idr { Some(r.read_ue()?) } else { None };
    let poc_lsb = if sps.poc_type == 0 { Some(r.read_bits(sps.log2_max_poc_lsb)?) } else { None };
    let delta_poc_bottom = if sps.poc_type == 0 && pps.bottom_field_pic_order_in_frame_present {
        Some(r.read_se()?)
    } else {
        None
    };
    let redundant_pic_cnt = if pps.redundant_pic_cnt_present { Some(r.read_ue()?) } else { None };
    let num_ref_idx_l0_active_override = if slice_type == SliceType::P {
        let flag = r.read_bit()?;
        if flag == 1 {
            Some(r.read_ue()? + 1)
        } else {
            None
        }
    } else {
        None
    };
    if slice_type != SliceType::I {
        let list_mod = r.read_bit()?;
        if list_mod == 1 {
            return Err(H264Error::Unsupported("ref_pic_list_modification"));
        }
    }
    if slice_type == SliceType::P && pps.weighted_pred {
        return Err(H264Error::Unsupported("weighted_pred"));
    }
    let mut no_output = false;
    let mut long_term = false;
    if nal_ref_idc != 0 {
        if is_idr {
            no_output = r.read_bit()? == 1;
            long_term = r.read_bit()? == 1;
        } else {
            let adaptive = r.read_bit()?;
            if adaptive == 1 {
                return Err(H264Error::Unsupported("adaptive_ref_pic_marking"));
            }
        }
    }
    let cabac_init_idc = if pps.cabac && slice_type != SliceType::I {
        Some(r.read_ue()?)
    } else {
        None
    };
    let slice_qp_delta = r.read_se()?;
    let mut disable_deblocking = 0u32;
    let mut alpha = 0i32;
    let mut beta = 0i32;
    if pps.deblocking_filter_control_present {
        disable_deblocking = r.read_ue()?;
        if disable_deblocking != 1 {
            alpha = r.read_se()?;
            beta = r.read_se()?;
        }
    }
    let header_bits = r.pos_bits();
    Ok(SliceHeader {
        nal_unit_type,
        nal_ref_idc,
        first_mb_in_slice: first_mb,
        slice_type,
        pps_id,
        frame_num,
        idr_pic_id,
        poc_lsb,
        delta_poc_bottom,
        redundant_pic_cnt,
        num_ref_idx_l0_active_override,
        cabac_init_idc,
        slice_qp_delta,
        disable_deblocking_filter_idc: disable_deblocking,
        slice_alpha_c0_offset_div2: alpha,
        slice_beta_offset_div2: beta,
        no_output_of_prior_pics: no_output,
        long_term_reference: long_term,
        header_bits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../tests/data/sample.h264");

    /// Pisarz bitów do budowy ręcznych strumieni w testach.
    struct BitWriter {
        bytes: Vec<u8>,
        nbits: usize,
    }

    impl BitWriter {
        fn new() -> Self {
            BitWriter { bytes: Vec::new(), nbits: 0 }
        }
        fn bit(&mut self, b: u32) -> &mut Self {
            if self.nbits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if b != 0 {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.nbits % 8);
            }
            self.nbits += 1;
            self
        }
        fn bits(&mut self, v: u32, n: u32) -> &mut Self {
            for i in (0..n).rev() {
                self.bit((v >> i) & 1);
            }
            self
        }
        fn ue(&mut self, v: u32) -> &mut Self {
            let x = v as u64 + 1;
            let len = 64 - x.leading_zeros();
            self.bits(0, len - 1);
            for i in (0..len).rev() {
                self.bit(((x >> i) & 1) as u32);
            }
            self
        }
        fn se(&mut self, v: i32) -> &mut Self {
            self.ue(if v > 0 { (2 * v - 1) as u32 } else { (-2 * v) as u32 })
        }
        fn finish(&mut self) -> Vec<u8> {
            self.bit(1);
            while !self.nbits.is_multiple_of(8) {
                self.bit(0);
            }
            self.bytes.clone()
        }
    }

    fn nals() -> Vec<&'static [u8]> {
        split_annexb(SAMPLE)
    }

    fn types() -> Vec<(u8, u8)> {
        nals().iter().map(|n| nal_header(n).map(|(r, t)| (t, r)).unwrap()).collect()
    }

    fn sample_sps() -> Sps {
        let n = nals()[0];
        parse_sps(&unescape_rbsp(n)).unwrap()
    }

    fn sample_pps() -> Pps {
        parse_pps(&unescape_rbsp(nals()[1])).unwrap()
    }

    // ---------- BitReader ----------

    #[test]
    fn bit_reader_reads_bits_ue_and_se_per_the_spec_tables() {
        // 1 | 010 | 011 | 00100 | 00101 | 00110 | 00111 = ue 0,1,2,3,4,5,6
        let mut w = BitWriter::new();
        for v in 0..7 {
            w.ue(v);
        }
        let d = w.finish();
        let mut r = BitReader::new(&d);
        for v in 0..7 {
            assert_eq!(r.read_ue(), Ok(v));
        }
        // se: 0, 1, -1, 2, -2
        let mut w = BitWriter::new();
        for v in [0, 1, -1, 2, -2, 100, -100] {
            w.se(v);
        }
        let d = w.finish();
        let mut r = BitReader::new(&d);
        for v in [0, 1, -1, 2, -2, 100, -100] {
            assert_eq!(r.read_se(), Ok(v));
        }
        let mut r = BitReader::new(&[0b1010_0000, 0xFF]);
        assert_eq!(r.read_bit(), Ok(1));
        assert_eq!(r.read_bits(3), Ok(0b010));
        assert_eq!(r.pos_bits(), 4);
        assert_eq!(r.read_bits(0), Ok(0));
        assert_eq!(r.read_bits(12), Ok(0b0000_1111_1111));
        assert_eq!(r.bits_left(), 0);
        assert_eq!(r.read_bit(), Err(H264Error::Truncated));
    }

    #[test]
    fn bit_reader_handles_32_bit_reads_limits_and_hostile_golomb_codes() {
        let mut r = BitReader::new(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(r.read_bits(32), Ok(0xDEAD_BEEF));
        assert_eq!(BitReader::new(&[0; 8]).read_bits(33), Err(H264Error::Invalid("read_bits > 32")));
        assert_eq!(BitReader::new(&[0; 8]).read_ue(), Err(H264Error::Invalid("ue too long")), "32 zer wiodących");
        assert_eq!(BitReader::new(&[0x00]).read_ue(), Err(H264Error::Truncated), "same zera i koniec danych");
        assert_eq!(BitReader::new(&[]).read_ue(), Err(H264Error::Truncated));
        // Największe dozwolone ue: 31 zer, 1, 31 bitów jedynek = 2^32 - 2.
        let mut w = BitWriter::new();
        w.bits(0, 31).bit(1).bits(0x7FFF_FFFF, 31);
        let d = w.finish();
        assert_eq!(BitReader::new(&d).read_ue(), Ok(u32::MAX - 1));
    }

    // ---------- NAL ----------

    #[test]
    fn annexb_splitting_handles_three_and_four_byte_start_codes_and_trailing_zeros() {
        let d = [9, 9, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 0, 0, 0, 1, 0x65, 4, 5];
        let n = split_annexb(&d);
        assert_eq!(n, vec![&[0x67u8, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
        assert!(split_annexb(&[]).is_empty());
        assert!(split_annexb(&[1, 2, 3, 4]).is_empty(), "bez kodu startowego nie ma NAL-i");
        assert!(split_annexb(&[0, 0, 1]).is_empty(), "pusty NAL pominięty");
        assert!(split_annexb(&[0, 0, 1, 0, 0, 1]).is_empty());
    }

    #[test]
    fn sample_stream_splits_into_the_expected_nal_sequence() {
        // (typ, nal_ref_idc): SPS, PPS, SEI, IDR, P, P, SPS, PPS, IDR, P, P
        let want = [(7, 3), (8, 3), (6, 0), (5, 3), (1, 2), (1, 2), (7, 3), (8, 3), (5, 3), (1, 2), (1, 2)];
        assert_eq!(types(), want.to_vec());
        let sizes: Vec<usize> = nals().iter().map(|n| n.len()).collect();
        assert_eq!(sizes, vec![21, 5, 629, 780, 7, 7, 21, 5, 839, 53, 7]);
    }

    #[test]
    fn nal_header_validates_the_forbidden_bit() {
        assert_eq!(nal_header(&[0x65, 1]), Ok((3, 5)));
        assert_eq!(nal_header(&[0x41]), Ok((2, 1)));
        assert_eq!(nal_header(&[0xE5]), Err(H264Error::Invalid("forbidden_zero_bit")));
        assert_eq!(nal_header(&[]), Err(H264Error::Truncated));
    }

    #[test]
    fn unescape_removes_emulation_prevention_bytes_only_where_the_pattern_applies() {
        assert_eq!(unescape_rbsp(&[0x65, 0, 0, 3, 1, 0xFF]), vec![0, 0, 1, 0xFF]);
        assert_eq!(unescape_rbsp(&[0x65, 0, 0, 3, 0, 0, 3, 3]), vec![0, 0, 0, 0, 3], "dwa kolejne wzorce; ostatnie 03 po 00 00 usunięte, kolejne 03 zostaje");
        assert_eq!(unescape_rbsp(&[0x65, 1, 2, 3]), vec![1, 2, 3], "03 bez 00 00 przed sobą zostaje");
        assert_eq!(unescape_rbsp(&[0x65, 0, 3, 1]), vec![0, 3, 1], "jedno zero to nie wzorzec");
        assert_eq!(unescape_rbsp(&[0x65]), Vec::<u8>::new());
        assert_eq!(unescape_rbsp(&[]), Vec::<u8>::new());
        assert_eq!(unescape_rbsp(&[0x65, 0, 0, 3]), vec![0, 0], "03 na końcu usunięte");
    }

    #[test]
    fn escaped_bit_offset_counts_the_header_and_earlier_emulation_bytes() {
        let nal = [0x65, 0, 0, 3, 1, 0xFF]; // RBSP: 00 00 01 FF
        assert_eq!(escaped_bit_offset(&nal, 0), 8, "tylko nagłówek");
        assert_eq!(escaped_bit_offset(&nal, 15), 8 + 15, "wewnątrz bajtu 1: bajt 03 jeszcze nie minął");
        assert_eq!(escaped_bit_offset(&nal, 16), 8 + 16 + 8, "początek bajtu 2: 03 leży przed nim");
        assert_eq!(escaped_bit_offset(&nal, 24), 8 + 24 + 8);
        assert_eq!(escaped_bit_offset(&[0x65, 1, 2, 3], 16), 8 + 16, "bez emulacji");
    }

    // ---------- SPS / PPS ----------

    #[test]
    fn real_sps_from_libx264_is_parsed() {
        let s = sample_sps();
        assert_eq!((s.profile_idc, s.level_idc, s.sps_id), (77, 10, 0));
        assert_eq!((s.chroma_format_idc, s.log2_max_frame_num, s.poc_type), (1, 4, 2));
        assert_eq!((s.max_num_ref_frames, s.width_mbs, s.height_mbs), (1, 4, 3));
        assert!(s.direct_8x8_inference && s.vui_present);
        assert_eq!(s.crop, (0, 0, 0, 0));
        assert_eq!(s.size(), (64, 48));
    }

    #[test]
    fn real_pps_from_libx264_is_parsed() {
        let p = sample_pps();
        assert_eq!((p.pps_id, p.sps_id), (0, 0));
        assert!(p.cabac && !p.weighted_pred && !p.bottom_field_pic_order_in_frame_present);
        // x264 ustawia domyślnie chroma_qp_index_offset = -2 (potwierdzone przez ffmpeg trace_headers).
        assert_eq!((p.num_ref_idx_l0_default_active, p.pic_init_qp, p.chroma_qp_index_offset), (1, 23, -2));
        assert!(p.deblocking_filter_control_present && !p.constrained_intra_pred && !p.redundant_pic_cnt_present);
        assert!(!p.transform_8x8_mode, "profil Main nie ma transform_8x8");
    }

    fn crafted_sps(profile: u8, poc_type: u32, frame_mbs_only: bool, crop: bool) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.bits(profile as u32, 8).bits(0, 8).bits(31, 8).ue(0);
        if profile == 100 {
            w.ue(1).ue(0).ue(0).bit(0).bit(0);
        }
        w.ue(4).ue(poc_type);
        if poc_type == 0 {
            w.ue(2);
        }
        w.ue(2).bit(0).ue(119).ue(67).bit(frame_mbs_only as u32);
        if !frame_mbs_only {
            w.bit(0);
        }
        w.bit(1);
        if crop {
            w.bit(1).ue(0).ue(0).ue(0).ue(4);
        } else {
            w.bit(0);
        }
        w.bit(0);
        w.finish()
    }

    #[test]
    fn crafted_sps_covers_high_profile_poc_type_0_and_cropping() {
        let s = parse_sps(&crafted_sps(100, 0, true, true)).unwrap();
        assert_eq!((s.profile_idc, s.level_idc, s.log2_max_frame_num, s.poc_type, s.log2_max_poc_lsb), (100, 31, 8, 0, 6));
        assert_eq!((s.max_num_ref_frames, s.width_mbs, s.height_mbs), (2, 120, 68));
        assert_eq!(s.crop, (0, 0, 0, 8), "1920x1088 z przycięciem 4*2 próbki od dołu => 1920x1080");
        assert_eq!(s.size(), (1920, 1080));
        assert!(!s.vui_present);
        let s = parse_sps(&crafted_sps(77, 2, true, false)).unwrap();
        assert_eq!((s.poc_type, s.log2_max_poc_lsb, s.size()), (2, 0, (1920, 1088)));
    }

    #[test]
    fn unsupported_sps_features_are_rejected_not_misparsed() {
        assert_eq!(parse_sps(&crafted_sps(77, 1, true, false)), Err(H264Error::Unsupported("pic_order_cnt_type 1")));
        assert_eq!(parse_sps(&crafted_sps(77, 2, false, false)), Err(H264Error::Unsupported("frame_mbs_only_flag 0")));
        assert!(matches!(parse_sps(&crafted_sps(88, 2, true, false)), Err(H264Error::Unsupported(_))), "profil Extended");
        assert!(matches!(parse_sps(&crafted_sps(244, 2, true, false)), Err(H264Error::Unsupported(_))), "High 4:4:4");
        // High z chroma_format_idc 2 (4:2:2).
        let mut w = BitWriter::new();
        w.bits(100, 8).bits(0, 8).bits(31, 8).ue(0).ue(2).ue(0).ue(0).bit(0).bit(0).ue(4).ue(2).ue(1).bit(0).ue(3).ue(2).bit(1).bit(1).bit(0).bit(0);
        assert!(matches!(parse_sps(&w.finish()), Err(H264Error::Unsupported(_))));
        // High z macierzami skalującymi.
        let mut w = BitWriter::new();
        w.bits(100, 8).bits(0, 8).bits(31, 8).ue(0).ue(1).ue(0).ue(0).bit(0).bit(1);
        assert!(matches!(parse_sps(&w.finish()), Err(H264Error::Unsupported(_))));
    }

    #[test]
    fn every_truncation_of_real_headers_is_an_error_and_never_a_panic() {
        for (idx, parse_ok) in [(0usize, true), (1, false)] {
            let rbsp = unescape_rbsp(nals()[idx]);
            for cut in 0..rbsp.len() {
                let r_sps = parse_sps(&rbsp[..cut]);
                let r_pps = parse_pps(&rbsp[..cut]);
                if parse_ok {
                    // Parser kończy na fladze VUI i nie czyta VUI, więc ucięcie wewnątrz VUI nie jest błędem; wcześniej musi być.
                    assert!(r_sps.is_err() || cut >= 6, "SPS ucięty do {cut}/{}", rbsp.len());
                } else {
                    assert!(r_pps.is_err() || cut >= rbsp.len() - 1, "PPS ucięty do {cut}/{}", rbsp.len());
                }
            }
        }
    }

    // ---------- slice header ----------

    fn slice(i: usize) -> (SliceHeader, Vec<u8>) {
        let n = nals()[i];
        let (idc, t) = nal_header(n).unwrap();
        let rbsp = unescape_rbsp(n);
        (parse_slice_header(&rbsp, t, idc, &sample_sps(), &sample_pps()).unwrap(), rbsp)
    }

    #[test]
    fn idr_slice_header_matches_the_stream_and_a_hand_computed_bit_length() {
        let (h, _) = slice(3);
        assert_eq!((h.nal_unit_type, h.nal_ref_idc, h.first_mb_in_slice, h.slice_type, h.pps_id), (5, 3, 0, SliceType::I, 0));
        assert_eq!((h.frame_num, h.idr_pic_id, h.poc_lsb), (0, Some(0), None));
        assert_eq!((h.slice_qp_delta, h.disable_deblocking_filter_idc, h.cabac_init_idc), (8, 0, None));
        assert!(!h.no_output_of_prior_pics && !h.long_term_reference);
        // first_mb 1 + slice_type(7) 7 + pps 1 + frame_num 4 + idr_pic_id 1 + marking 2 + qp_delta se(8) 9 + deblock idc 1 + alpha/beta 2.
        assert_eq!(h.header_bits, 28);
    }

    #[test]
    fn p_slice_headers_and_frame_numbers_follow_the_stream() {
        let (p1, _) = slice(4);
        assert_eq!((p1.nal_unit_type, p1.nal_ref_idc, p1.slice_type, p1.frame_num, p1.idr_pic_id), (1, 2, SliceType::P, 1, None));
        assert_eq!((p1.num_ref_idx_l0_active_override, p1.cabac_init_idc, p1.slice_qp_delta), (None, Some(0), 8));
        // first_mb 1 + slice_type(5) 5 + pps 1 + frame_num 4 + override flag 1 + list mod flag 1 + marking flag 1 + cabac_init_idc 1 + qp_delta 9 + deblock 1 + alpha/beta 2.
        assert_eq!(p1.header_bits, 27);
        let frame_nums: Vec<u32> = [3usize, 4, 5, 8, 9, 10].iter().map(|&i| slice(i).0.frame_num).collect();
        assert_eq!(frame_nums, vec![0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn slice_header_rejects_wrong_nal_types_and_mismatched_pps() {
        let (sps, pps) = (sample_sps(), sample_pps());
        let rbsp = unescape_rbsp(nals()[3]);
        assert!(matches!(parse_slice_header(&rbsp, 2, 3, &sps, &pps), Err(H264Error::Invalid(_))), "typ 2 (partycja A) poza profilem");
        assert!(matches!(parse_slice_header(&rbsp, 7, 3, &sps, &pps), Err(H264Error::Invalid(_))));
        let mut other = pps.clone();
        other.pps_id = 5;
        assert!(matches!(parse_slice_header(&rbsp, 5, 3, &sps, &other), Err(H264Error::Invalid(_))), "pps_id w nagłówku różni się od przekazanego PPS");
    }

    /// Ręcznie zbudowany nagłówek slice'a P (nie-IDR, poc_type 2, CABAC) z wybranymi odstępstwami.
    fn p_header(slice_type: u32, list_mod: bool, mmco: bool) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.ue(0).ue(slice_type).ue(0).bits(1, 4); // first_mb, slice_type, pps, frame_num
        w.bit(0); // num_ref_idx_active_override_flag
        if slice_type % 5 != 2 {
            w.bit(list_mod as u32);
        }
        w.bit(mmco as u32); // adaptive_ref_pic_marking_mode_flag (nal_ref_idc != 0)
        if slice_type % 5 != 2 {
            w.ue(0); // cabac_init_idc
        }
        w.se(8).ue(0).se(0).se(0);
        w.finish()
    }

    #[test]
    fn hand_built_p_header_parses_and_unsupported_variants_are_rejected() {
        let (sps, pps) = (sample_sps(), sample_pps());
        let ok = parse_slice_header(&p_header(5, false, false), 1, 2, &sps, &pps).unwrap();
        assert_eq!((ok.slice_type, ok.frame_num, ok.header_bits), (SliceType::P, 1, 27));
        assert_eq!(parse_slice_header(&p_header(0, false, false), 1, 2, &sps, &pps).unwrap().slice_type, SliceType::P, "typ 0 = P (bez +5)");
    }

    #[test]
    fn b_slices_list_modification_and_mmco_are_unsupported() {
        let (sps, pps) = (sample_sps(), sample_pps());
        assert!(matches!(parse_slice_header(&p_header(6, false, false), 1, 2, &sps, &pps), Err(H264Error::Unsupported(_))), "B");
        assert!(matches!(parse_slice_header(&p_header(1, false, false), 1, 2, &sps, &pps), Err(H264Error::Unsupported(_))), "B");
        assert!(matches!(parse_slice_header(&p_header(3, false, false), 1, 2, &sps, &pps), Err(H264Error::Unsupported(_))), "SP");
        assert!(matches!(parse_slice_header(&p_header(4, false, false), 1, 2, &sps, &pps), Err(H264Error::Unsupported(_))), "SI");
        assert!(matches!(parse_slice_header(&p_header(5, true, false), 1, 2, &sps, &pps), Err(H264Error::Unsupported(_))), "modyfikacja list");
        assert!(matches!(parse_slice_header(&p_header(5, false, true), 1, 2, &sps, &pps), Err(H264Error::Unsupported(_))), "MMCO");
        assert!(matches!(parse_slice_header(&p_header(10, false, false), 1, 2, &sps, &pps), Err(H264Error::Invalid(_))), "slice_type > 9");
    }

    #[test]
    fn random_bytes_never_panic_in_any_parser() {
        let (sps, pps) = (sample_sps(), sample_pps());
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let len = (next() % 64) as usize;
            let b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = parse_sps(&b);
            let _ = parse_pps(&b);
            for t in [1u8, 5] {
                let _ = parse_slice_header(&b, t, 2, &sps, &pps);
            }
            let _ = split_annexb(&b);
            let _ = unescape_rbsp(&b);
            let _ = escaped_bit_offset(&b, (next() % 600) as usize);
            let mut r = BitReader::new(&b);
            let _ = r.read_ue();
            let _ = r.read_se();
        }
    }
}
