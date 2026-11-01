//! Dekodowanie sprzętowe porównane z `ffmpeg` (dekoder programowy) na tej samej próbce; test pomijany bez VA-API lub ffmpeg.

use std::process::Command;

use gpu_vaapi::VaDecoder;

fn access_units(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut aus = Vec::new();
    let mut cur = Vec::new();
    for nal in gpu_h264::split_annexb(stream) {
        cur.extend_from_slice(&[0, 0, 0, 1]);
        cur.extend_from_slice(nal);
        let t = nal[0] & 31;
        if t == 1 || t == 5 {
            aus.push(std::mem::take(&mut cur));
        }
    }
    aus
}

fn ffmpeg_bgra(path: &str) -> Option<Vec<u8>> {
    let o = Command::new("ffmpeg").args(["-v", "error", "-i", path, "-f", "rawvideo", "-pix_fmt", "bgra", "-"]).output().ok()?;
    o.status.success().then_some(o.stdout)
}

fn compare(path: &str, w: usize, h: usize) {
    let Ok(mut dec) = VaDecoder::new() else {
        eprintln!("POMINIĘTO: brak VA-API");
        return;
    };
    let Some(reference) = ffmpeg_bgra(path) else {
        eprintln!("POMINIĘTO: brak ffmpeg");
        return;
    };
    let stream = std::fs::read(path).unwrap();
    let aus = access_units(&stream);
    assert_eq!(reference.len(), aus.len() * w * h * 4, "ffmpeg dał inną liczbę klatek");
    for (i, au) in aus.iter().enumerate() {
        let f = dec.decode(au).unwrap_or_else(|e| panic!("klatka {i}: {e}")).expect("klatka");
        assert_eq!((f.width as usize, f.height as usize), (w, h));
        let r = &reference[i * w * h * 4..(i + 1) * w * h * 4];
        let sum: u64 = f.bgra.iter().zip(r).map(|(a, b)| (*a as i64 - *b as i64).unsigned_abs()).sum();
        let mean = sum as f64 / r.len() as f64;
        assert!(mean < 3.0, "klatka {i}: średnia różnica {mean:.2} względem ffmpeg");
    }
}

#[test]
fn x264_sample_matches_ffmpeg() {
    compare(concat!(env!("CARGO_MANIFEST_DIR"), "/../gpu-h264/tests/data/sample.h264"), 64, 48);
}

#[test]
fn nvenc_streams_match_ffmpeg_when_present() {
    let p = "/tmp/claude-1000/deleg/t360.h264";
    if std::path::Path::new(p).exists() {
        compare(p, 640, 360);
    }
}
