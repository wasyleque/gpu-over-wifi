//! Strojenie w trakcie sesji (bez wiedzy o grze): przełączniki opcji zachowujących poprawność danych (odczyt różnicowy gorących okien, kanał odczytów, pobranie
//! leniwe bez blokady) są przełączane w locie, a ICD mierzy własny FPS (klatki oddane przez `vkQueuePresentKHR` w oknach czasu) i na przemian porównuje
//! wariant bieżący z przeciwnym. Przeciwny zostaje, gdy wygrywa o więcej niż `MARGIN`; ocena powtarza się co `RECHECK`. Zmienna `GPU_ICD_HOTPUSH`/`GPU_ICD_READCHAN`/
//! `GPU_ICD_LAZY_UNLOCK` ustawiona jawnie na 0/1 przypina przełącznik; `GPU_ICD_TUNER=0` wyłącza strojenie (zostają wartości domyślne: wszystko włączone).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const N: usize = 3;
pub const NAMES: [&str; N] = ["odczyt różnicowy gorących okien", "kanał odczytów", "pobranie leniwe bez blokady"];
const ENVS: [&str; N] = ["GPU_ICD_HOTPUSH", "GPU_ICD_READCHAN", "GPU_ICD_LAZY_UNLOCK"];

static STATE: [AtomicBool; N] = [AtomicBool::new(true), AtomicBool::new(true), AtomicBool::new(true)];
pub static SWITCHES: AtomicU64 = AtomicU64::new(0);

/// Okno pomiarowe i parametry decyzji.
const WINDOW: Duration = Duration::from_secs(3);
const WARMUP: Duration = Duration::from_secs(25);
const ROUNDS: usize = 2; // par okien (bieżący, przeciwny) na jeden test
const MARGIN: f64 = 0.05;
const RECHECK: Duration = Duration::from_secs(300);

/// Przypięte zmienne środowiskowe obowiązują od pierwszego użycia przełącznika (nie dopiero od pierwszej klatki).
fn ensure_pins() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for i in 0..N {
            if let Some(v) = pinned(i) {
                STATE[i].store(v, Ordering::Relaxed);
            }
        }
    });
}

pub fn hotpush() -> bool {
    ensure_pins();
    STATE[0].load(Ordering::Relaxed)
}
pub fn readchan() -> bool {
    ensure_pins();
    STATE[1].load(Ordering::Relaxed)
}
pub fn lazy_unlock() -> bool {
    ensure_pins();
    STATE[2].load(Ordering::Relaxed)
}

/// Czy przeciwny wariant (średnia `b` FPS z `nb` okien) wygrywa z bieżącym (`a`, `na` okien) o więcej niż `margin`.
pub fn decide_flip(a: f64, na: u32, b: f64, nb: u32, margin: f64) -> bool {
    na > 0 && nb > 0 && b > a * (1.0 + margin)
}

fn pinned(i: usize) -> Option<bool> {
    std::env::var(ENVS[i]).ok().map(|v| v != "0")
}

fn tuner_on() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("GPU_ICD_TUNER").map(|v| v != "0").unwrap_or(true))
}

struct Tun {
    start: Instant,
    win_start: Instant,
    frames: u64,
    /// Który przełącznik testujemy, ile okien już zmierzono w tym teście i sumy FPS wariantu bieżącego (0) i przeciwnego (1).
    sw: usize,
    step: usize,
    sum: [f64; 2],
    cnt: [u32; 2],
    /// Wartość przełącznika przed testem (do powrotu/zatwierdzenia).
    orig: bool,
    idle_until: Option<Instant>,
    init: bool,
}

static TUN: Mutex<Option<Tun>> = Mutex::new(None);

/// Wołać przy każdym `vkQueuePresentKHR` gry (przed blokadą instancji).
pub fn on_present() {
    if !tuner_on() {
        return;
    }
    let now = Instant::now();
    let mut g = TUN.lock().unwrap_or_else(|e| e.into_inner());
    ensure_pins();
    let t = g.get_or_insert_with(|| {
        Tun { start: now, win_start: now, frames: 0, sw: 0, step: 0, sum: [0.0; 2], cnt: [0; 2], orig: true, idle_until: None, init: false }
    });
    t.frames += 1;
    if now.duration_since(t.start) < WARMUP {
        t.win_start = now;
        t.frames = 0;
        return;
    }
    let el = now.duration_since(t.win_start);
    if el < WINDOW {
        return;
    }
    let fps = t.frames as f64 / el.as_secs_f64();
    t.win_start = now;
    t.frames = 0;
    if let Some(until) = t.idle_until {
        if now < until {
            return;
        }
        t.idle_until = None;
        t.sw = 0;
        t.step = 0;
        t.init = false;
    }
    // Pomijamy przypięte przełączniki.
    while t.sw < N && pinned(t.sw).is_some() {
        t.sw += 1;
    }
    if t.sw >= N {
        t.idle_until = Some(now + RECHECK);
        return;
    }
    if !t.init {
        // Początek testu przełącznika: bieżąca wartość to wariant 0; pierwsze okno (już zmierzone) po zmianie nie jest liczone.
        t.orig = STATE[t.sw].load(Ordering::Relaxed);
        t.sum = [0.0; 2];
        t.cnt = [0; 2];
        t.step = 0;
        t.init = true;
        return;
    }
    // Kroki parzyste mierzyły wariant bieżący, nieparzyste przeciwny; pierwsze okno po przełączeniu odrzucamy (ustalanie się obiegów).
    let variant = t.step % 2;
    if t.step >= 2 || variant == 0 {
        t.sum[variant] += fps;
        t.cnt[variant] += 1;
    }
    t.step += 1;
    if t.step < 2 * ROUNDS + 1 {
        // Przełącz na wariant następnego okna.
        let want = if t.step % 2 == 0 { t.orig } else { !t.orig };
        STATE[t.sw].store(want, Ordering::Relaxed);
        return;
    }
    // Decyzja.
    let (a, b) = (t.sum[0] / t.cnt[0].max(1) as f64, t.sum[1] / t.cnt[1].max(1) as f64);
    let flip = decide_flip(a, t.cnt[0], b, t.cnt[1], MARGIN);
    let keep = if flip { !t.orig } else { t.orig };
    STATE[t.sw].store(keep, Ordering::Relaxed);
    if flip {
        SWITCHES.fetch_add(1, Ordering::Relaxed);
    }
    eprintln!(
        "[gpu-icd] strojenie: {} — bieżący {:.1} FPS, przeciwny {:.1} FPS → {}",
        NAMES[t.sw],
        a,
        b,
        if flip { if keep { "WŁĄCZONY" } else { "WYŁĄCZONY" } } else if keep { "zostaje włączony" } else { "zostaje wyłączony" }
    );
    t.sw += 1;
    t.init = false;
    if t.sw >= N {
        t.idle_until = Some(now + RECHECK);
    }
}

/// Stan przełączników do raportu końcowego.
pub fn report() -> String {
    format!(
        "{}; zmian decyzji: {}",
        (0..N).map(|i| format!("{} {}", NAMES[i], if STATE[i].load(Ordering::Relaxed) { "tak" } else { "nie" })).collect::<Vec<_>>().join(", "),
        SWITCHES.load(Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flips_only_when_the_opposite_variant_wins_by_more_than_the_margin() {
        assert!(decide_flip(15.0, 3, 25.0, 3, 0.05));
        assert!(!decide_flip(25.0, 3, 25.5, 3, 0.05), "różnica 2% to szum");
        assert!(!decide_flip(25.0, 3, 15.0, 3, 0.05), "gorszy wariant nigdy nie wygrywa");
        assert!(!decide_flip(15.0, 0, 25.0, 3, 0.05), "brak pomiarów bieżącego wariantu");
        assert!(!decide_flip(15.0, 3, 25.0, 0, 0.05), "brak pomiarów przeciwnego wariantu");
        assert!(decide_flip(20.0, 1, 21.01, 1, 0.05));
        assert!(!decide_flip(20.0, 1, 21.0, 1, 0.05), "dokładnie na progu nie przełącza");
    }
}
