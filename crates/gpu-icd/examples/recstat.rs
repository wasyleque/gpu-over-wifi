//! Analiza nagrania zapisów pamięci (`GPU_ICD_REC_DIR`): `recstat <plik> [maks. rekordów]`.
//! Odtwarza lustro (jak klient: prefiks = poprzednia treść zakresu) i porównuje rozmiary po kodowaniu:
//! zstd z prefiksem (dziś), XOR z poprzednią treścią + zstd, rozdzielenie bajtów słów 32-bitowych + zstd z prefiksem, oraz sam rozmiar zmienionych bajtów.
//! Liczy tylko rekordy po rozgrzewce (pierwsze 20% rekordów służy do zbudowania lustra).
use std::io::Read;

fn zstd_pfx(raw: &[u8], prefix: &[u8], level: i32) -> usize {
    let mut cctx = zstd::zstd_safe::CCtx::create();
    let mut out: Vec<u8> = Vec::with_capacity(zstd::zstd_safe::compress_bound(raw.len()));
    let ok = cctx.set_parameter(zstd::zstd_safe::CParameter::CompressionLevel(level)).is_ok() && cctx.ref_prefix(prefix).is_ok();
    if ok && cctx.compress2(&mut out, raw).is_ok() {
        out.len().min(raw.len())
    } else {
        raw.len()
    }
}

fn zstd_plain(raw: &[u8], level: i32) -> usize {
    zstd::bulk::compress(raw, level).map_or(raw.len(), |c| c.len().min(raw.len()))
}

/// Bajty słów 32-bitowych rozdzielone na 4 strumienie (bajt 0 wszystkich słów, potem bajt 1, ...).
fn planes(d: &[u8]) -> Vec<u8> {
    let w = d.len() / 4;
    let mut o = vec![0u8; d.len()];
    for i in 0..w {
        for k in 0..4 {
            o[k * w + i] = d[i * 4 + k];
        }
    }
    o[w * 4..].copy_from_slice(&d[w * 4..]);
    o
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("plik");
    let max: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let mut f = std::fs::File::open(&path).expect("otwarcie");
    let mut all = Vec::new();
    f.read_to_end(&mut all).expect("odczyt");
    let mut recs: Vec<(u64, &[u8])> = Vec::new();
    let mut at = 0usize;
    let mut size = 0u64;
    while at + 20 <= all.len() && recs.len() < max {
        let off = u64::from_le_bytes(all[at + 8..at + 16].try_into().unwrap());
        let len = u32::from_le_bytes(all[at + 16..at + 20].try_into().unwrap()) as usize;
        if at + 20 + len > all.len() {
            break;
        }
        recs.push((off, &all[at + 20..at + 20 + len]));
        size = size.max(off + len as u64);
        at += 20 + len;
    }
    println!("rekordów {}, rozmiar pamięci >= {} MB, bajtów {} MB", recs.len(), size >> 20, all.len() >> 20);
    let mut mirror = vec![0u8; size as usize];
    let warm = recs.len() / 5;
    // sumy: surowo, zmienione bajty, zstd-pfx(1), zstd-pfx(3), xor+zstd, planes+zstd-pfx, planes z xor
    let mut t = [0u64; 8];
    let mut t_ns = [0u128; 5];
    let mut rows = 0u64;
    for (i, (off, d)) in recs.iter().enumerate() {
        let (o, e) = (*off as usize, *off as usize + d.len());
        if i >= warm {
            let prefix = mirror[o..e].to_vec();
            let changed = prefix.iter().zip(d.iter()).filter(|(a, b)| a != b).count();
            t[0] += d.len() as u64;
            t[1] += changed as u64;
            let a = std::time::Instant::now();
            t[2] += zstd_pfx(d, &prefix, 1) as u64;
            t_ns[0] += a.elapsed().as_nanos();
            t[3] += zstd_pfx(d, &prefix, 3) as u64;
            let x: Vec<u8> = prefix.iter().zip(d.iter()).map(|(a, b)| a ^ b).collect();
            let a = std::time::Instant::now();
            t[4] += zstd_plain(&x, 1) as u64;
            t_ns[2] += a.elapsed().as_nanos();
            let pd = planes(d);
            let pp = planes(&prefix);
            let a = std::time::Instant::now();
            t[5] += zstd_pfx(&pd, &pp, 1) as u64;
            t_ns[3] += a.elapsed().as_nanos();
            t[6] += zstd_plain(&planes(&x), 1) as u64;
            // najmniejszy z kandydatów (górna granica wyboru per rekord)
            let best = zstd_pfx(d, &prefix, 1).min(zstd_plain(&x, 1)).min(zstd_pfx(&pd, &pp, 1));
            t[7] += best as u64;
            rows += 1;
        }
        mirror[o..e].copy_from_slice(d);
    }
    let mb = |v: u64| v as f64 / 1e6;
    println!("zmierzone rekordy: {rows}");
    println!("surowo            {:9.1} MB", mb(t[0]));
    println!("zmienione bajty   {:9.1} MB ({:.1}% surowych)", mb(t[1]), 100.0 * t[1] as f64 / t[0] as f64);
    println!("zstd1 + prefiks   {:9.1} MB  (dziś)  {:.0} ms", mb(t[2]), t_ns[0] as f64 / 1e6);
    println!("zstd3 + prefiks   {:9.1} MB", mb(t[3]));
    println!("xor + zstd1       {:9.1} MB  {:.0} ms", mb(t[4]), t_ns[2] as f64 / 1e6);
    println!("planes+zstd1 pfx  {:9.1} MB  {:.0} ms", mb(t[5]), t_ns[3] as f64 / 1e6);
    println!("planes xor zstd1  {:9.1} MB", mb(t[6]));
    println!("min z trzech (górna granica)  {:9.1} MB", mb(t[7]));
}
