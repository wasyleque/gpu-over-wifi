//! Koder H.264 na NVENC z niskim opóźnieniem: wejście to klatki BGRA (bufor CUDA w formacie ARGB, konwersję kolorów robi NVENC),
//! wyjście to strumień Annex B (jedna jednostka dostępu na ramkę, SPS/PPS powtarzane przy każdym IDR, jeden slice, bez B-ramek).

use std::ffi::c_void;

use crate::api::{ck, Nvenc, Session};
use crate::consts::*;
use crate::cuda::{Cuda, CuContext};
use crate::sys::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Średni i maksymalny bitrate (CBR) w bitach na sekundę.
    pub bitrate_bps: u32,
}

impl EncoderConfig {
    /// Sprawdza zakres wartości (NVENC H.264: wymiary parzyste, 33..=4096).
    pub fn validate(&self) -> Result<(), String> {
        if self.width < 33 || self.height < 33 || self.width > 4096 || self.height > 4096 || self.width % 2 != 0 || self.height % 2 != 0 {
            return Err(format!("wymiary {}x{} poza zakresem H.264 NVENC (parzyste, 33..=4096)", self.width, self.height));
        }
        if self.fps == 0 || self.fps > 240 {
            return Err(format!("fps {} poza zakresem 1..=240", self.fps));
        }
        if self.bitrate_bps < 100_000 || self.bitrate_bps > 800_000_000 {
            return Err(format!("bitrate {} b/s poza zakresem 0,1..800 Mb/s", self.bitrate_bps));
        }
        Ok(())
    }

    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

pub struct H264Encoder {
    cuda: Cuda,
    ctx: CuContext,
    session: Session,
    cfg: EncoderConfig,
    devptr: u64,
    registered: NV_ENC_REGISTERED_PTR,
    bitstream: NV_ENC_OUTPUT_PTR,
    frame_idx: u32,
}

// SAFETY: koder używany z jednego wątku naraz (kontekst CUDA ustawiany jako bieżący przy każdym kodowaniu).
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    pub fn new(nv: &Nvenc, cfg: EncoderConfig) -> Result<H264Encoder, String> {
        cfg.validate()?;
        let cuda = Cuda::load()?;
        let ctx = cuda.create_context(0)?;
        let session = nv.open_session(ctx)?;
        let enc = session.enc;

        // Preset P1 + strojenie ultra niskich opóźnień jako punkt wyjścia.
        let get_preset = session.fl.nvEncGetEncodePresetConfigEx.ok_or("brak nvEncGetEncodePresetConfigEx")?;
        let mut preset = NV_ENC_PRESET_CONFIG { version: NV_ENC_PRESET_CONFIG_VER, presetCfg: NV_ENC_CONFIG { version: NV_ENC_CONFIG_VER, ..Default::default() }, ..Default::default() };
        // SAFETY: struktury mają ustawione wersje; sesja jest ważna.
        ck("nvEncGetEncodePresetConfigEx", unsafe { get_preset(enc, NV_ENC_CODEC_H264_GUID, NV_ENC_PRESET_P1_GUID, NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY, &mut preset) })?;
        let mut config = preset.presetCfg;
        config.profileGUID = NV_ENC_H264_PROFILE_HIGH_GUID;
        config.gopLength = NVENC_INFINITE_GOPLENGTH; // IDR wyłącznie na żądanie
        config.frameIntervalP = 1; // bez B-ramek
        config.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
        config.rcParams.averageBitRate = cfg.bitrate_bps;
        config.rcParams.maxBitRate = cfg.bitrate_bps;
        config.rcParams.vbvBufferSize = cfg.bitrate_bps / cfg.fps; // bufor na jedną klatkę: stałe opóźnienie
        config.rcParams.vbvInitialDelay = cfg.bitrate_bps / cfg.fps;
        config.rcParams.multiPass = NV_ENC_MULTI_PASS_DISABLED;
        config.rcParams.set_enableLookahead(0);
        config.rcParams.set_zeroReorderDelay(1);
        // SAFETY: `encodeCodecConfig` to unia; ustawiamy wariant H.264 zgodnie z `encodeGUID`.
        unsafe {
            let h = &mut config.encodeCodecConfig.h264Config;
            h.idrPeriod = NVENC_INFINITE_GOPLENGTH;
            h.maxNumRefFrames = 1;
            h.sliceMode = 3; // stała liczba slice'ów na ramkę
            h.sliceModeData = 1; // jeden slice
            h.entropyCodingMode = NV_ENC_H264_ENTROPY_CODING_MODE_CABAC;
            h.chromaFormatIDC = 1;
            h.set_repeatSPSPPS(1);
            h.set_outputAUD(0);
            h.set_enableIntraRefresh(0);
        }

        let mut init = NV_ENC_INITIALIZE_PARAMS {
            version: NV_ENC_INITIALIZE_PARAMS_VER,
            encodeGUID: NV_ENC_CODEC_H264_GUID,
            presetGUID: NV_ENC_PRESET_P1_GUID,
            encodeWidth: cfg.width,
            encodeHeight: cfg.height,
            darWidth: cfg.width,
            darHeight: cfg.height,
            frameRateNum: cfg.fps,
            frameRateDen: 1,
            enablePTD: 1,
            encodeConfig: &mut config,
            maxEncodeWidth: cfg.width,
            maxEncodeHeight: cfg.height,
            tuningInfo: NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            bufferFormat: NV_ENC_BUFFER_FORMAT_ARGB,
            ..Default::default()
        };
        let initialize = session.fl.nvEncInitializeEncoder.ok_or("brak nvEncInitializeEncoder")?;
        // SAFETY: `init` wskazuje na `config`, który żyje do końca wywołania.
        ck("nvEncInitializeEncoder", unsafe { initialize(enc, &mut init) })?;

        let create_bs = session.fl.nvEncCreateBitstreamBuffer.ok_or("brak nvEncCreateBitstreamBuffer")?;
        let mut bs = NV_ENC_CREATE_BITSTREAM_BUFFER { version: NV_ENC_CREATE_BITSTREAM_BUFFER_VER, ..Default::default() };
        // SAFETY: struktura ma wersję.
        ck("nvEncCreateBitstreamBuffer", unsafe { create_bs(enc, &mut bs) })?;

        let devptr = cuda.alloc(cfg.frame_bytes())?;
        let register = session.fl.nvEncRegisterResource.ok_or("brak nvEncRegisterResource")?;
        let mut reg = NV_ENC_REGISTER_RESOURCE {
            version: NV_ENC_REGISTER_RESOURCE_VER,
            resourceType: NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
            width: cfg.width,
            height: cfg.height,
            pitch: cfg.width * 4,
            resourceToRegister: devptr as *mut c_void,
            bufferFormat: NV_ENC_BUFFER_FORMAT_ARGB,
            bufferUsage: NV_ENC_INPUT_IMAGE,
            ..Default::default()
        };
        // SAFETY: `devptr` to ważna alokacja CUDA o rozmiarze klatki.
        ck("nvEncRegisterResource", unsafe { register(enc, &mut reg) })?;
        Ok(H264Encoder { cuda, ctx, session, cfg, devptr, registered: reg.registeredResource, bitstream: bs.bitstreamBuffer, frame_idx: 0 })
    }

    pub fn config(&self) -> EncoderConfig {
        self.cfg
    }

    /// Koduje jedną klatkę BGRA (`width * height * 4` B). Pierwsza klatka i klatki z `force_idr` są IDR z SPS/PPS.
    /// Zwraca strumień Annex B tej klatki.
    pub fn encode(&mut self, bgra: &[u8], force_idr: bool) -> Result<Vec<u8>, String> {
        if bgra.len() != self.cfg.frame_bytes() {
            return Err(format!("zły rozmiar klatki: {} B zamiast {}", bgra.len(), self.cfg.frame_bytes()));
        }
        self.cuda.set_current(self.ctx)?;
        self.cuda.copy_to_device(self.devptr, bgra)?;
        let enc = self.session.enc;
        let fl = self.session.fl;
        let map = fl.nvEncMapInputResource.ok_or("brak nvEncMapInputResource")?;
        let unmap = fl.nvEncUnmapInputResource.ok_or("brak nvEncUnmapInputResource")?;
        let encode = fl.nvEncEncodePicture.ok_or("brak nvEncEncodePicture")?;
        let lock = fl.nvEncLockBitstream.ok_or("brak nvEncLockBitstream")?;
        let unlock = fl.nvEncUnlockBitstream.ok_or("brak nvEncUnlockBitstream")?;

        let mut m = NV_ENC_MAP_INPUT_RESOURCE { version: NV_ENC_MAP_INPUT_RESOURCE_VER, registeredResource: self.registered, ..Default::default() };
        // SAFETY: zarejestrowany zasób jest ważny.
        ck("nvEncMapInputResource", unsafe { map(enc, &mut m) })?;
        let idr = force_idr || self.frame_idx == 0;
        let mut pic = NV_ENC_PIC_PARAMS {
            version: NV_ENC_PIC_PARAMS_VER,
            inputWidth: self.cfg.width,
            inputHeight: self.cfg.height,
            inputPitch: self.cfg.width * 4,
            encodePicFlags: if idr { NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS } else { 0 },
            frameIdx: self.frame_idx,
            inputTimeStamp: self.frame_idx as u64,
            inputBuffer: m.mappedResource,
            outputBitstream: self.bitstream,
            bufferFmt: m.mappedBufferFmt,
            pictureStruct: NV_ENC_PIC_STRUCT_FRAME,
            ..Default::default()
        };
        // SAFETY: `pic` odwołuje się do zmapowanego wejścia i bufora wyjściowego, oba ważne.
        let st = unsafe { encode(enc, &mut pic) };
        let result = if st == 0 {
            let mut lk = NV_ENC_LOCK_BITSTREAM { version: NV_ENC_LOCK_BITSTREAM_VER, outputBitstream: self.bitstream, ..Default::default() };
            // SAFETY: blokujemy bufor wyjściowy dla tej ramki (tryb synchroniczny).
            let s2 = unsafe { lock(enc, &mut lk) };
            if s2 == 0 {
                // SAFETY: `bitstreamBufferPtr` wskazuje na `bitstreamSizeInBytes` bajtów ważnych do odblokowania.
                let data = unsafe { std::slice::from_raw_parts(lk.bitstreamBufferPtr as *const u8, lk.bitstreamSizeInBytes as usize) }.to_vec();
                // SAFETY: bufor był zablokowany powyżej.
                unsafe { unlock(enc, self.bitstream) };
                Ok(data)
            } else {
                ck("nvEncLockBitstream", s2).map(|_| Vec::new())
            }
        } else {
            ck("nvEncEncodePicture", st).map(|_| Vec::new())
        };
        // SAFETY: zasób był zmapowany powyżej.
        unsafe { unmap(enc, m.mappedResource) };
        self.frame_idx = self.frame_idx.wrapping_add(1);
        result
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        let _ = self.cuda.set_current(self.ctx);
        let fl = self.session.fl;
        // SAFETY: zasoby pochodzą z `new` i są zwalniane raz, przed zniszczeniem sesji i kontekstu.
        unsafe {
            if let Some(f) = fl.nvEncUnregisterResource {
                f(self.session.enc, self.registered);
            }
            if let Some(f) = fl.nvEncDestroyBitstreamBuffer {
                f(self.session.enc, self.bitstream);
            }
        }
        self.cuda.free(self.devptr);
        // `session` zniszczy kodera w swoim Drop; kontekst CUDA niszczymy po nim w kolejności pól nie gwarantowanej, więc:
        std::mem::drop(std::mem::replace(&mut self.session, Session { fl, enc: std::ptr::null_mut() }));
        self.cuda.destroy_context(self.ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EncoderConfig {
        EncoderConfig { width: 640, height: 360, fps: 60, bitrate_bps: 20_000_000 }
    }

    #[test]
    fn config_validation_rejects_out_of_range_values() {
        assert!(cfg().validate().is_ok());
        assert_eq!(cfg().frame_bytes(), 640 * 360 * 4);
        for bad in [
            EncoderConfig { width: 0, ..cfg() },
            EncoderConfig { width: 641, ..cfg() },
            EncoderConfig { height: 5000, ..cfg() },
            EncoderConfig { fps: 0, ..cfg() },
            EncoderConfig { fps: 500, ..cfg() },
            EncoderConfig { bitrate_bps: 1000, ..cfg() },
            EncoderConfig { bitrate_bps: u32::MAX, ..cfg() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    /// Wzór testowy: gradient z ruchomym prostokątem (żeby ramki P miały co kodować).
    fn pattern(c: EncoderConfig, t: usize) -> Vec<u8> {
        let (w, h) = (c.width as usize, c.height as usize);
        let mut f = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let inside = x >= (t * 8) % (w - 64) && x < (t * 8) % (w - 64) + 64 && y >= 100 && y < 164;
                f[i] = if inside { 255 } else { (x * 255 / w) as u8 }; // B
                f[i + 1] = if inside { 255 } else { (y * 255 / h) as u8 }; // G
                f[i + 2] = if inside { 0 } else { 128 }; // R
                f[i + 3] = 255;
            }
        }
        f
    }

    #[test]
    fn real_encoder_produces_a_stream_our_own_parser_accepts_when_nvenc_is_present() {
        let Ok(nv) = Nvenc::load() else {
            eprintln!("POMINIĘTO: brak NVENC");
            return;
        };
        let Ok(mut e) = H264Encoder::new(&nv, cfg()) else {
            eprintln!("POMINIĘTO: nie udało się utworzyć kodera (brak GPU NVIDIA?)");
            return;
        };
        let mut frames = Vec::new();
        for t in 0..10 {
            frames.push(e.encode(&pattern(cfg(), t), t == 5).expect("encode"));
        }
        assert!(frames.iter().all(|f| !f.is_empty()));
        // Pierwsza klatka i klatka wymuszona to IDR z SPS i PPS; pozostałe to P.
        for (i, f) in frames.iter().enumerate() {
            let nals = gpu_h264::split_annexb(f);
            let types: Vec<u8> = nals.iter().map(|n| gpu_h264::nal_header(n).unwrap().1).collect();
            let idr = i == 0 || i == 5;
            assert_eq!(types.contains(&5), idr, "klatka {i}: typy {types:?}");
            assert_eq!(types.contains(&7) && types.contains(&8), idr, "SPS/PPS tylko przy IDR (klatka {i}): {types:?}");
            assert_eq!(types.iter().filter(|t| **t == 1 || **t == 5).count(), 1, "jeden slice na ramkę (klatka {i}): {types:?}");
        }
        // Nasz parser czyta SPS/PPS i nagłówki wszystkich slice'ów (profil jest dokładnie tym, czego oczekuje dekoder).
        let first = gpu_h264::split_annexb(&frames[0]);
        let sps = gpu_h264::parse_sps(&gpu_h264::unescape_rbsp(first.iter().find(|n| gpu_h264::nal_header(n).unwrap().1 == 7).unwrap())).expect("SPS");
        let pps = gpu_h264::parse_pps(&gpu_h264::unescape_rbsp(first.iter().find(|n| gpu_h264::nal_header(n).unwrap().1 == 8).unwrap())).expect("PPS");
        assert_eq!((sps.size(), sps.max_num_ref_frames, sps.profile_idc), ((640, 360), 1, 100));
        assert!(pps.cabac);
        for (i, f) in frames.iter().enumerate() {
            for n in gpu_h264::split_annexb(f) {
                let (idc, t) = gpu_h264::nal_header(n).unwrap();
                if t == 1 || t == 5 {
                    let h = gpu_h264::parse_slice_header(&gpu_h264::unescape_rbsp(n), t, idc, &sps, &pps).unwrap_or_else(|er| panic!("klatka {i}: {er:?}"));
                    assert_eq!(h.first_mb_in_slice, 0);
                    assert_eq!(h.slice_type == gpu_h264::SliceType::I, t == 5, "klatka {i}");
                }
            }
        }
    }

    #[test]
    fn wrong_frame_size_is_an_error() {
        let Ok(nv) = Nvenc::load() else { return };
        let Ok(mut e) = H264Encoder::new(&nv, cfg()) else { return };
        assert!(e.encode(&[0u8; 10], false).is_err());
    }
}
