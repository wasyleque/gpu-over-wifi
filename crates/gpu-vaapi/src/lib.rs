//! Sprzętowy dekoder H.264 przez VA-API (`libva.so.2`, `libva-drm.so.2` ładowane `dlopen`). Obsługuje strumień, który sami
//! produkujemy (patrz `gpu-h264`): ramki I/P, jeden slice, bez B-ramek i pól. Wyjście: klatki BGRA.

#![allow(clippy::missing_transmute_annotations)]

#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code, clippy::all, rustdoc::all)]
pub mod sys;
mod dl;
mod nv12;

use std::ffi::{c_char, c_int, c_void, CStr};

use gpu_h264::{Pps, SliceHeader, SliceType, Sps};

use crate::dl::Lib;
use crate::sys::*;

pub use nv12::nv12_to_bgra;

type Dpy = *mut c_void;

/// Zdekodowana klatka (BGRA, po przycięciu).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

struct Fns {
    get_display_drm: unsafe extern "C" fn(c_int) -> Dpy,
    initialize: unsafe extern "C" fn(Dpy, *mut c_int, *mut c_int) -> i32,
    terminate: unsafe extern "C" fn(Dpy) -> i32,
    create_config: unsafe extern "C" fn(Dpy, i32, i32, *mut VAConfigAttrib, c_int, *mut u32) -> i32,
    destroy_config: unsafe extern "C" fn(Dpy, u32) -> i32,
    create_surfaces: unsafe extern "C" fn(Dpy, u32, u32, u32, *mut u32, u32, *mut c_void, u32) -> i32,
    destroy_surfaces: unsafe extern "C" fn(Dpy, *mut u32, c_int) -> i32,
    create_context: unsafe extern "C" fn(Dpy, u32, c_int, c_int, c_int, *mut u32, c_int, *mut u32) -> i32,
    destroy_context: unsafe extern "C" fn(Dpy, u32) -> i32,
    create_buffer: unsafe extern "C" fn(Dpy, u32, i32, u32, u32, *mut c_void, *mut u32) -> i32,
    destroy_buffer: unsafe extern "C" fn(Dpy, u32) -> i32,
    begin_picture: unsafe extern "C" fn(Dpy, u32, u32) -> i32,
    render_picture: unsafe extern "C" fn(Dpy, u32, *mut u32, c_int) -> i32,
    end_picture: unsafe extern "C" fn(Dpy, u32) -> i32,
    sync_surface: unsafe extern "C" fn(Dpy, u32) -> i32,
    derive_image: unsafe extern "C" fn(Dpy, u32, *mut VAImage) -> i32,
    destroy_image: unsafe extern "C" fn(Dpy, u32) -> i32,
    map_buffer: unsafe extern "C" fn(Dpy, u32, *mut *mut c_void) -> i32,
    unmap_buffer: unsafe extern "C" fn(Dpy, u32) -> i32,
    error_str: unsafe extern "C" fn(i32) -> *const c_char,
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {{
        let p = $lib.sym($name).ok_or_else(|| format!("libva: brak symbolu {}", $name))?;
        // SAFETY: sygnatura symbolu pochodzi z nagłówków libva.
        unsafe { std::mem::transmute::<*mut c_void, _>(p) }
    }};
}

#[derive(Clone, Copy)]
struct RefPic {
    surface: u32,
    frame_num: u32,
    poc: i32,
}

struct Sized {
    context: u32,
    surfaces: Vec<u32>,
    width_mbs: u32,
    height_mbs: u32,
}

pub struct VaDecoder {
    f: Fns,
    dpy: Dpy,
    _drm: std::fs::File,
    config: u32,
    sized: Option<Sized>,
    sps: Option<Sps>,
    pps: Option<Pps>,
    refs: Vec<RefPic>,
    next_surface: usize,
    poc: i32,
}

// SAFETY: wyświetlacz VA używany z jednego wątku naraz (wymaga `&mut self`).
unsafe impl Send for VaDecoder {}

impl VaDecoder {
    /// Otwiera pierwsze działające urządzenie renderujące z obsługą dekodowania H.264 High/Main.
    pub fn new() -> Result<VaDecoder, String> {
        let va = Lib::open(&["libva.so.2", "libva.so"])?;
        let drm = Lib::open(&["libva-drm.so.2", "libva-drm.so"])?;
        let f = Fns {
            get_display_drm: sym!(drm, "vaGetDisplayDRM"),
            initialize: sym!(va, "vaInitialize"),
            terminate: sym!(va, "vaTerminate"),
            create_config: sym!(va, "vaCreateConfig"),
            destroy_config: sym!(va, "vaDestroyConfig"),
            create_surfaces: sym!(va, "vaCreateSurfaces"),
            destroy_surfaces: sym!(va, "vaDestroySurfaces"),
            create_context: sym!(va, "vaCreateContext"),
            destroy_context: sym!(va, "vaDestroyContext"),
            create_buffer: sym!(va, "vaCreateBuffer"),
            destroy_buffer: sym!(va, "vaDestroyBuffer"),
            begin_picture: sym!(va, "vaBeginPicture"),
            render_picture: sym!(va, "vaRenderPicture"),
            end_picture: sym!(va, "vaEndPicture"),
            sync_surface: sym!(va, "vaSyncSurface"),
            derive_image: sym!(va, "vaDeriveImage"),
            destroy_image: sym!(va, "vaDestroyImage"),
            map_buffer: sym!(va, "vaMapBuffer"),
            unmap_buffer: sym!(va, "vaUnmapBuffer"),
            error_str: sym!(va, "vaErrorStr"),
        };
        let mut last = "brak urządzeń /dev/dri/renderD*".to_string();
        let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri").map_err(|e| e.to_string())?.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("renderD"))).collect();
        nodes.sort();
        for path in nodes {
            let file = match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
                Ok(x) => x,
                Err(e) => {
                    last = format!("{}: {e}", path.display());
                    continue;
                }
            };
            use std::os::fd::AsRawFd;
            // SAFETY: deskryptor jest ważny, dopóki trzymamy `file`.
            let dpy = unsafe { (f.get_display_drm)(file.as_raw_fd()) };
            if dpy.is_null() {
                continue;
            }
            let (mut maj, mut min) = (0, 0);
            // SAFETY: poprawne wskaźniki wyjściowe.
            if unsafe { (f.initialize)(dpy, &mut maj, &mut min) } != 0 {
                last = format!("{}: vaInitialize nie powiodło się", path.display());
                continue;
            }
            let mut attr = VAConfigAttrib { type_: VAConfigAttribRTFormat, value: VA_RT_FORMAT_YUV420 };
            let mut cfg = 0u32;
            for profile in [VAProfileH264High, VAProfileH264Main] {
                // SAFETY: atrybut i wskaźnik wyjściowy są poprawne.
                if unsafe { (f.create_config)(dpy, profile as i32, VAEntrypointVLD as i32, &mut attr, 1, &mut cfg) } == 0 {
                    return Ok(VaDecoder { f, dpy, _drm: file, config: cfg, sized: None, sps: None, pps: None, refs: Vec::new(), next_surface: 0, poc: 0 });
                }
            }
            last = format!("{}: brak dekodowania H.264 (VLD)", path.display());
            // SAFETY: wyświetlacz zainicjowany powyżej.
            unsafe { (f.terminate)(dpy) };
        }
        Err(last)
    }

    fn ck(&self, what: &str, st: i32) -> Result<(), String> {
        if st == 0 {
            return Ok(());
        }
        // SAFETY: `vaErrorStr` zwraca statyczny napis.
        let msg = unsafe { CStr::from_ptr((self.f.error_str)(st)) }.to_string_lossy().into_owned();
        Err(format!("{what}: {msg} ({st})"))
    }

    fn ensure_sized(&mut self, sps: &Sps) -> Result<(), String> {
        if let Some(s) = &self.sized {
            if s.width_mbs == sps.width_mbs && s.height_mbs == sps.height_mbs {
                return Ok(());
            }
        }
        self.free_sized();
        let (w, h) = (sps.width_mbs * 16, sps.height_mbs * 16);
        let n = sps.max_num_ref_frames as usize + 3;
        let mut surfaces = vec![0u32; n];
        // SAFETY: bufor ma miejsce na `n` identyfikatorów.
        self.ck("vaCreateSurfaces", unsafe { (self.f.create_surfaces)(self.dpy, VA_RT_FORMAT_YUV420, w, h, surfaces.as_mut_ptr(), n as u32, std::ptr::null_mut(), 0) })?;
        let mut context = 0u32;
        // SAFETY: powierzchnie powstały powyżej.
        let st = unsafe { (self.f.create_context)(self.dpy, self.config, w as c_int, h as c_int, 1, surfaces.as_mut_ptr(), n as c_int, &mut context) };
        if st != 0 {
            // SAFETY: zwalniamy powierzchnie utworzone powyżej.
            unsafe { (self.f.destroy_surfaces)(self.dpy, surfaces.as_mut_ptr(), n as c_int) };
            return self.ck("vaCreateContext", st);
        }
        self.sized = Some(Sized { context, surfaces, width_mbs: sps.width_mbs, height_mbs: sps.height_mbs });
        self.refs.clear();
        self.next_surface = 0;
        Ok(())
    }

    fn free_sized(&mut self) {
        if let Some(mut s) = self.sized.take() {
            // SAFETY: obiekty pochodzą z `ensure_sized`.
            unsafe {
                (self.f.destroy_context)(self.dpy, s.context);
                (self.f.destroy_surfaces)(self.dpy, s.surfaces.as_mut_ptr(), s.surfaces.len() as c_int);
            }
        }
    }

    /// Dekoduje jedną jednostkę dostępu (Annex B). Zwraca klatkę, gdy zawierała slice obrazu.
    pub fn decode(&mut self, annexb: &[u8]) -> Result<Option<Frame>, String> {
        let mut out = None;
        for nal in gpu_h264::split_annexb(annexb) {
            let (idc, t) = gpu_h264::nal_header(nal).map_err(|e| format!("{e:?}"))?;
            match t {
                7 => self.sps = Some(gpu_h264::parse_sps(&gpu_h264::unescape_rbsp(nal)).map_err(|e| format!("SPS: {e:?}"))?),
                8 => self.pps = Some(gpu_h264::parse_pps(&gpu_h264::unescape_rbsp(nal)).map_err(|e| format!("PPS: {e:?}"))?),
                1 | 5 => out = Some(self.decode_slice(nal, idc, t)?),
                _ => {}
            }
        }
        Ok(out)
    }

    fn decode_slice(&mut self, nal: &[u8], idc: u8, t: u8) -> Result<Frame, String> {
        let sps = self.sps.clone().ok_or("slice przed SPS")?;
        let pps = self.pps.clone().ok_or("slice przed PPS")?;
        let rbsp = gpu_h264::unescape_rbsp(nal);
        let sh = gpu_h264::parse_slice_header(&rbsp, t, idc, &sps, &pps).map_err(|e| format!("nagłówek slice'a: {e:?}"))?;
        self.ensure_sized(&sps)?;
        if t == 5 {
            self.refs.clear();
            self.poc = 0;
        }
        let max_frame_num = 1u32 << sps.log2_max_frame_num;
        let wrap = |r: &RefPic| if r.frame_num > sh.frame_num { r.frame_num as i64 - max_frame_num as i64 } else { r.frame_num as i64 };
        let mut ordered = self.refs.clone();
        ordered.sort_by_key(|r| std::cmp::Reverse(wrap(r)));

        let (context, target) = {
            let s = self.sized.as_ref().ok_or("brak kontekstu")?;
            // Powierzchnia docelowa: pierwsza, która nie jest referencją.
            let n = s.surfaces.len();
            let mut pick = None;
            for k in 0..n {
                let c = s.surfaces[(self.next_surface + k) % n];
                if !self.refs.iter().any(|r| r.surface == c) {
                    pick = Some((self.next_surface + k) % n);
                    break;
                }
            }
            let i = pick.ok_or("brak wolnej powierzchni")?;
            self.next_surface = (i + 1) % n;
            (s.context, s.surfaces[i])
        };
        self.poc += 2;
        let pic = |r: &RefPic| VAPictureH264 { picture_id: r.surface, frame_idx: r.frame_num, flags: VA_PICTURE_H264_SHORT_TERM_REFERENCE, TopFieldOrderCnt: r.poc, BottomFieldOrderCnt: r.poc, va_reserved: [0; 4] };
        let invalid = VAPictureH264 { picture_id: VA_INVALID_SURFACE, flags: VA_PICTURE_H264_INVALID, ..Default::default() };

        let mut pp = VAPictureParameterBufferH264 {
            CurrPic: VAPictureH264 { picture_id: target, frame_idx: sh.frame_num, flags: 0, TopFieldOrderCnt: self.poc, BottomFieldOrderCnt: self.poc, va_reserved: [0; 4] },
            ReferenceFrames: [invalid; 16],
            picture_width_in_mbs_minus1: (sps.width_mbs - 1) as u16,
            picture_height_in_mbs_minus1: (sps.height_mbs - 1) as u16,
            num_ref_frames: sps.max_num_ref_frames as u8,
            pic_init_qp_minus26: (pps.pic_init_qp - 26) as i8,
            chroma_qp_index_offset: pps.chroma_qp_index_offset as i8,
            second_chroma_qp_index_offset: pps.chroma_qp_index_offset as i8,
            frame_num: sh.frame_num as u16,
            ..Default::default()
        };
        for (i, r) in ordered.iter().take(16).enumerate() {
            pp.ReferenceFrames[i] = pic(r);
        }
        let seq = (sps.chroma_format_idc & 3) | (1 << 4) /* frame_mbs_only */ | ((sps.direct_8x8_inference as u32) << 6) | ((sps.log2_max_frame_num - 4) << 8) | (sps.poc_type << 12) | (sps.log2_max_poc_lsb.saturating_sub(4) << 14);
        pp.seq_fields.value = seq;
        let fields = (pps.cabac as u32) | ((pps.weighted_pred as u32) << 1) | ((pps.transform_8x8_mode as u32) << 4) | ((pps.constrained_intra_pred as u32) << 6) | ((pps.bottom_field_pic_order_in_frame_present as u32) << 7) | ((pps.deblocking_filter_control_present as u32) << 8) | ((pps.redundant_pic_cnt_present as u32) << 9) | (((idc != 0) as u32) << 10);
        pp.pic_fields.value = fields;

        let iq = VAIQMatrixBufferH264 { ScalingList4x4: [[16; 16]; 6], ScalingList8x8: [[16; 64]; 2], va_reserved: [0; 4] };

        let n_active = sh.num_ref_idx_l0_active_override.unwrap_or(pps.num_ref_idx_l0_default_active);
        let mut sp = VASliceParameterBufferH264 {
            slice_data_size: nal.len() as u32,
            slice_data_offset: 0,
            slice_data_flag: VA_SLICE_DATA_FLAG_ALL,
            slice_data_bit_offset: gpu_h264::escaped_bit_offset(nal, sh.header_bits) as u16,
            first_mb_in_slice: sh.first_mb_in_slice as u16,
            slice_type: if sh.slice_type == SliceType::I { 2 } else { 0 },
            num_ref_idx_l0_active_minus1: n_active.saturating_sub(1) as u8,
            cabac_init_idc: sh.cabac_init_idc.unwrap_or(0) as u8,
            slice_qp_delta: sh.slice_qp_delta as i8,
            disable_deblocking_filter_idc: sh.disable_deblocking_filter_idc as u8,
            slice_alpha_c0_offset_div2: sh.slice_alpha_c0_offset_div2 as i8,
            slice_beta_offset_div2: sh.slice_beta_offset_div2 as i8,
            RefPicList0: [invalid; 32],
            RefPicList1: [invalid; 32],
            ..Default::default()
        };
        for (i, r) in ordered.iter().take(32).enumerate() {
            sp.RefPicList0[i] = pic(r);
        }

        let mut bufs = Vec::new();
        let mk = |me: &Self, ty: i32, size: usize, data: *mut c_void, bufs: &mut Vec<u32>| -> Result<(), String> {
            let mut id = 0u32;
            // SAFETY: `data` wskazuje na `size` ważnych bajtów; sterownik kopiuje dane.
            me.ck("vaCreateBuffer", unsafe { (me.f.create_buffer)(me.dpy, context, ty, size as u32, 1, data, &mut id) })?;
            bufs.push(id);
            Ok(())
        };
        let mut data = nal.to_vec();
        let res = (|| -> Result<(), String> {
            mk(self, VAPictureParameterBufferType as i32, std::mem::size_of_val(&pp), &mut pp as *mut _ as *mut c_void, &mut bufs)?;
            let mut iq = iq;
            mk(self, VAIQMatrixBufferType as i32, std::mem::size_of_val(&iq), &mut iq as *mut _ as *mut c_void, &mut bufs)?;
            mk(self, VASliceParameterBufferType as i32, std::mem::size_of_val(&sp), &mut sp as *mut _ as *mut c_void, &mut bufs)?;
            mk(self, VASliceDataBufferType as i32, data.len(), data.as_mut_ptr() as *mut c_void, &mut bufs)?;
            // SAFETY: kontekst i powierzchnia docelowa są ważne; bufory utworzone powyżej.
            unsafe {
                self.ck("vaBeginPicture", (self.f.begin_picture)(self.dpy, context, target))?;
                self.ck("vaRenderPicture", (self.f.render_picture)(self.dpy, context, bufs.as_mut_ptr(), bufs.len() as c_int))?;
                self.ck("vaEndPicture", (self.f.end_picture)(self.dpy, context))?;
                self.ck("vaSyncSurface", (self.f.sync_surface)(self.dpy, target))?;
            }
            Ok(())
        })();
        for b in bufs {
            // SAFETY: bufory utworzone powyżej.
            unsafe { (self.f.destroy_buffer)(self.dpy, b) };
        }
        res?;

        if idc != 0 {
            if self.refs.len() >= sps.max_num_ref_frames.max(1) as usize {
                let oldest = (0..self.refs.len()).min_by_key(|&i| wrap(&self.refs[i])).unwrap_or(0);
                self.refs.remove(oldest);
            }
            self.refs.push(RefPic { surface: target, frame_num: sh.frame_num, poc: self.poc });
        }
        self.read_frame(target, &sps)
    }

    fn read_frame(&self, surface: u32, sps: &Sps) -> Result<Frame, String> {
        let mut img = VAImage::default();
        // SAFETY: powierzchnia jest zsynchronizowana; wskaźnik wyjściowy poprawny.
        self.ck("vaDeriveImage", unsafe { (self.f.derive_image)(self.dpy, surface, &mut img) })?;
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: bufor obrazu pochodzi z `vaDeriveImage`.
        let st = unsafe { (self.f.map_buffer)(self.dpy, img.buf, &mut p) };
        let result = if st == 0 && !p.is_null() {
            // SAFETY: `data_size` bajtów jest dostępne do odblokowania bufora.
            let mapped = unsafe { std::slice::from_raw_parts(p as *const u8, img.data_size as usize) };
            // Pamięć obrazu bywa niecache'owana: czytamy ją jednym sekwencyjnym kopiowaniem, nie bajt po bajcie.
            let data = mapped.to_vec();
            let (cl, cr, ct, cb) = sps.crop;
            let (full_w, full_h) = (sps.width_mbs * 16, sps.height_mbs * 16);
            let (w, h) = (full_w.saturating_sub(cl + cr), full_h.saturating_sub(ct + cb));
            let r = nv12_to_bgra(&data, img.pitches[0] as usize, img.offsets[0] as usize, img.pitches[1] as usize, img.offsets[1] as usize, (cl as usize, ct as usize), (w as usize, h as usize));
            // SAFETY: bufor był zmapowany powyżej.
            unsafe { (self.f.unmap_buffer)(self.dpy, img.buf) };
            r.map(|bgra| Frame { width: w, height: h, bgra })
        } else {
            self.ck("vaMapBuffer", st).and(Err("pusty wskaźnik".to_string()))
        };
        // SAFETY: obraz utworzony przez `vaDeriveImage`.
        unsafe { (self.f.destroy_image)(self.dpy, img.image_id) };
        result
    }
}

impl Drop for VaDecoder {
    fn drop(&mut self) {
        self.free_sized();
        // SAFETY: obiekty pochodzą z `new`.
        unsafe {
            (self.f.destroy_config)(self.dpy, self.config);
            (self.f.terminate)(self.dpy);
        }
    }
}

#[allow(dead_code)]
fn _unused(_: &SliceHeader) {}
