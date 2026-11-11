//! `gpu-route`: podgląd profili trasy (`GPU_ICD_ROUTE`, patrz `docs/DESIGN-route.md`) i decyzji, którą podjąłby tryb `auto`.
//!
//! `gpu-route` wypisuje wszystkie profile; `gpu-route --forget <klucz>` usuwa profil (aplikacja zostanie zmierzona od nowa).
//! Cel FPS jak w sterowniku: `GPU_ICD_ROUTE_TARGET_FPS` (domyślnie 60).

use gpu_icd::route::{decide, profile_dir, profile_path, sanitize_key, Inputs, Mode, Policy, Profile};

fn cell(p: &Profile, mode: Mode, version: &str) -> String {
    let Some(e) = p.entry(mode) else { return "brak".into() };
    let secs = e.hist.total_us() / 1_000_000;
    let stale = if mode == Mode::Remote && e.version != version { format!(", nieważny (wersja {})", e.version) } else { String::new() };
    match e.hist.percentile_us(75) {
        Some(us) if e.hist.sufficient() => format!("{:.1} FPS (p75 {:.1} ms, {secs} s{stale})", 1e6 / us as f64, us as f64 / 1e3),
        _ => format!("za mało danych ({secs} s{stale})"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, key] = args.as_slice() {
        if flag == "--forget" {
            let path = profile_path(&sanitize_key(key));
            match std::fs::remove_file(&path) {
                Ok(()) => println!("usunięto {}", path.display()),
                Err(e) => {
                    eprintln!("{}: {e}", path.display());
                    std::process::exit(1);
                }
            }
            return;
        }
    }
    if !args.is_empty() {
        eprintln!("użycie: gpu-route | gpu-route --forget <klucz>");
        std::process::exit(2);
    }
    let version = env!("CARGO_PKG_VERSION");
    let target_fps = std::env::var("GPU_ICD_ROUTE_TARGET_FPS").ok().and_then(|v| v.parse::<u64>().ok()).filter(|f| (1..=1000).contains(f)).unwrap_or(60);
    let dir = profile_dir();
    let mut files: Vec<_> = std::fs::read_dir(&dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "route")).collect()).unwrap_or_default();
    files.sort();
    println!("profile trasy w {} (gpu-icd {version}, cel {target_fps} FPS)", dir.display());
    if files.is_empty() {
        println!("brak profili: uruchom aplikację z GPU_ICD_ROUTE=auto (albo local/remote), co najmniej minutę gry");
    }
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let p = Profile::parse(&text);
        let inputs = Inputs {
            policy: Policy::Auto,
            local_ok: true,
            remote_ok: true,
            local_p75_us: p.p75_us(Mode::Local, version),
            remote_p75_us: p.p75_us(Mode::Remote, version),
            target_us: 1_000_000 / target_fps,
        };
        let (mode, reason) = decide(&inputs);
        println!("{}", f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        println!("  lokalnie: {}", cell(&p, Mode::Local, version));
        println!("  zdalnie:  {}", cell(&p, Mode::Remote, version));
        println!("  auto:     {mode:?} ({reason:?})");
    }
}
