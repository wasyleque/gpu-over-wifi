//! `nvenc-encode-test <szer> <wys> <klatki> <Mb/s> <plik.h264>`: koduje wzór testowy i mierzy czas oraz rozmiar klatek.

use gpu_nvenc::api::Nvenc;
use gpu_nvenc::encoder::{EncoderConfig, H264Encoder};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!("Użycie: nvenc-encode-test <szer> <wys> <klatki> <Mb/s> <plik.h264>");
        std::process::exit(2);
    }
    let (w, h, n, mbps): (u32, u32, usize, u32) = (a[1].parse().unwrap_or(1280), a[2].parse().unwrap_or(720), a[3].parse().unwrap_or(60), a[4].parse().unwrap_or(40));
    let cfg = EncoderConfig { width: w, height: h, fps: 60, bitrate_bps: mbps * 1_000_000 };
    let nv = Nvenc::load().unwrap_or_else(|e| {
        eprintln!("NVENC: {e}");
        std::process::exit(1);
    });
    let mut e = H264Encoder::new(&nv, cfg).unwrap_or_else(|er| {
        eprintln!("koder: {er}");
        std::process::exit(1);
    });
    let (wz, hz) = (w as usize, h as usize);
    let frame = |t: usize| -> Vec<u8> {
        let mut f = vec![0u8; wz * hz * 4];
        for y in 0..hz {
            for x in 0..wz {
                let i = (y * wz + x) * 4;
                let bx = (t * 9) % (wz - 96);
                let inside = x >= bx && x < bx + 96 && y >= hz / 4 && y < hz / 4 + 96;
                let checker = ((x / 32 + y / 32) % 2) as u8 * 40;
                f[i] = if inside { 255 } else { ((x * 255 / wz) as u8).saturating_add(checker) };
                f[i + 1] = if inside { 255 } else { ((y * 255 / hz) as u8).saturating_add(checker) };
                f[i + 2] = if inside { 0 } else { 128 };
                f[i + 3] = 255;
            }
        }
        f
    };
    let mut out = Vec::new();
    let mut times = Vec::new();
    let mut sizes = Vec::new();
    for t in 0..n {
        let f = frame(t);
        let s = std::time::Instant::now();
        let b = e.encode(&f, t == 0).unwrap_or_else(|er| {
            eprintln!("klatka {t}: {er}");
            std::process::exit(1);
        });
        times.push(s.elapsed().as_secs_f64() * 1000.0);
        sizes.push(b.len());
        out.extend_from_slice(&b);
    }
    std::fs::write(&a[5], &out).unwrap_or_else(|er| {
        eprintln!("zapis: {er}");
        std::process::exit(1);
    });
    let mut ts = times.clone();
    ts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let total: usize = sizes.iter().sum();
    println!("{w}x{h}, {n} klatek, cel {mbps} Mb/s: czas kodowania klatki (kopia CUDA + NVENC): mediana {:.2} ms, p95 {:.2} ms, maks. {:.2} ms", ts[ts.len() / 2], ts[ts.len() * 95 / 100], ts[ts.len() - 1]);
    println!("  rozmiar: IDR {} B, P średnio {} B, razem {} KB => {:.1} Mb/s przy 60 FPS", sizes[0], (total - sizes[0]) / (n - 1).max(1), total / 1000, total as f64 * 8.0 * 60.0 / n as f64 / 1e6);
}
