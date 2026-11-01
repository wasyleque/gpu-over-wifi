//! `vaapi-bench <plik.h264>`: dekoduje strumień Annex B (jedna jednostka dostępu = do pierwszego slice'a) i mierzy czasy.

use gpu_vaapi::VaDecoder;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Użycie: vaapi-bench <plik.h264>");
        std::process::exit(2);
    });
    let stream = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let mut dec = VaDecoder::new().unwrap_or_else(|e| {
        eprintln!("VA-API: {e}");
        std::process::exit(1);
    });
    let mut aus = Vec::new();
    let mut cur = Vec::new();
    for nal in gpu_h264::split_annexb(&stream) {
        cur.extend_from_slice(&[0, 0, 0, 1]);
        cur.extend_from_slice(nal);
        if matches!(nal[0] & 31, 1 | 5) {
            aus.push(std::mem::take(&mut cur));
        }
    }
    let mut times = Vec::new();
    let mut dim = (0, 0);
    for au in &aus {
        let t = std::time::Instant::now();
        match dec.decode(au) {
            Ok(Some(f)) => dim = (f.width, f.height),
            Ok(None) => {}
            Err(e) => {
                eprintln!("błąd dekodowania: {e}");
                std::process::exit(1);
            }
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!("{}x{}, {} klatek: dekodowanie + odczyt + BGRA: mediana {:.2} ms, p95 {:.2} ms, maks. {:.2} ms", dim.0, dim.1, aus.len(), times[times.len() / 2], times[times.len() * 95 / 100], times[times.len() - 1]);
}
