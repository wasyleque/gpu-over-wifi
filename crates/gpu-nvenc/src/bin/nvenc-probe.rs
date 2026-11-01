//! `nvenc-probe`: sprawdza na tej maszynie CUDA (kontekst, alokacja, kopia z hosta) oraz możliwości NVENC.

use gpu_nvenc::api::{version_string, Nvenc};
use gpu_nvenc::consts::*;
use gpu_nvenc::cuda::Cuda;
use gpu_nvenc::sys::*;

fn die(msg: &str) -> ! {
    eprintln!("BŁĄD: {msg}");
    std::process::exit(1);
}

fn main() {
    let cuda = Cuda::load().unwrap_or_else(|e| die(&format!("CUDA niedostępna: {e}")));
    let ctx = cuda.create_context(0).unwrap_or_else(|e| die(&format!("kontekst CUDA: {e}")));
    let (w, h) = (1920usize, 1080usize);
    let size = w * h * 4;
    let ptr = cuda.alloc(size).unwrap_or_else(|e| die(&e));
    let frame: Vec<u8> = (0..size).map(|i| i as u8).collect();
    let t = std::time::Instant::now();
    for _ in 0..50 {
        cuda.copy_to_device(ptr, &frame).unwrap_or_else(|e| die(&e));
    }
    let secs = t.elapsed().as_secs_f64() / 50.0;
    println!("CUDA: kopia klatki 1080p BGRA host -> GPU: {:.2} ms ({:.1} GB/s)", secs * 1000.0, size as f64 / secs / 1e9);
    cuda.free(ptr);

    let nv = Nvenc::load().unwrap_or_else(|e| die(&format!("NVENC niedostępny: {e}")));
    println!("NVENC: sterownik obsługuje API do {} (nagłówek {})", version_string(nv.max_api_version()), version_string(0xC2));
    let s = nv.open_session(ctx).unwrap_or_else(|e| die(&e));
    let names: [(&str, GUID); 3] = [("H.264", NV_ENC_CODEC_H264_GUID), ("HEVC", NV_ENC_CODEC_HEVC_GUID), ("AV1", NV_ENC_CODEC_AV1_GUID)];
    let guids = s.codec_guids().unwrap_or_else(|e| die(&e));
    for (n, g) in &names {
        let has = guids.contains(g);
        print!("  {n}: {}", if has { "TAK" } else { "nie" });
        if has {
            let cap = |c| s.cap(*g, c).map(|v| v.to_string()).unwrap_or_else(|e| e);
            print!(
                " | maks. {}x{}, B-ramki do {}, CABAC {}, intra refresh {}, dynamiczna zmiana rozdzielczości {}",
                cap(NV_ENC_CAPS_WIDTH_MAX),
                cap(NV_ENC_CAPS_HEIGHT_MAX),
                cap(NV_ENC_CAPS_NUM_MAX_BFRAMES),
                cap(NV_ENC_CAPS_SUPPORT_CABAC),
                cap(NV_ENC_CAPS_SUPPORT_INTRA_REFRESH),
                cap(NV_ENC_CAPS_SUPPORT_DYN_RES_CHANGE)
            );
        }
        println!();
    }
    let fmts = s.input_formats(NV_ENC_CODEC_H264_GUID).unwrap_or_else(|e| die(&e));
    let name = |f: u32| match f {
        NV_ENC_BUFFER_FORMAT_NV12 => "NV12",
        NV_ENC_BUFFER_FORMAT_ARGB => "ARGB",
        NV_ENC_BUFFER_FORMAT_ABGR => "ABGR",
        NV_ENC_BUFFER_FORMAT_IYUV => "IYUV",
        _ => "inny",
    };
    println!("  formaty wejściowe H.264: {:?}", fmts.iter().map(|f| name(*f)).collect::<Vec<_>>());
    drop(s);
    cuda.destroy_context(ctx);
}
