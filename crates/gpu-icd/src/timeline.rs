//! Oś czasu poleceń w oknie ~250 ms (`GPU_ICD_TIMELINE=<sekundy>`: okno startuje tyle sekund po pierwszym poleceniu, wynik trafia raz do logu).
//! Pokazuje, który wątek i na co czeka w obrębie kilku klatek (kto jest ostatnim ogniwem łańcucha oczekiwań). Bez zmiennej koszt to jeden odczyt atomu.
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_millis(250);
/// 0 = wyłączone, 1 = czeka na start okna, 2 = nagrywa, 3 = zakończone.
static PHASE: AtomicU8 = AtomicU8::new(0);
static INIT: std::sync::Once = std::sync::Once::new();
static T0: Mutex<Option<(Instant, Duration)>> = Mutex::new(None);
static EVENTS: Mutex<Vec<(u64, u64, String, &'static str, u64)>> = Mutex::new(Vec::new());

fn init() {
    INIT.call_once(|| {
        if let Some(s) = std::env::var("GPU_ICD_TIMELINE").ok().and_then(|v| v.parse::<f64>().ok()) {
            *T0.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), Duration::from_secs_f64(s)));
            PHASE.store(1, Relaxed);
        }
    });
}

thread_local! {
    /// Czas oczekiwania TEGO wątku na blokadę instancji w bieżącym poleceniu (µs); ustawia `fwd::forward_inner`.
    pub static LOCKW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static NAME: String = std::fs::read_to_string("/proc/thread-self/comm").map(|s| s.trim().to_string()).unwrap_or_default();
}

/// Czy nagrywamy teraz (wołane przed każdym poleceniem).
pub fn recording() -> bool {
    init();
    match PHASE.load(Relaxed) {
        0 | 3 => false,
        1 => {
            let mut g = T0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((t, after)) = *g {
                if t.elapsed() >= after {
                    *g = Some((Instant::now(), WINDOW));
                    PHASE.store(2, Relaxed);
                    return true;
                }
            }
            false
        }
        _ => {
            let (t, w) = T0.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or((Instant::now(), WINDOW));
            if t.elapsed() >= w + Duration::from_millis(100) {
                finish();
                return false;
            }
            true
        }
    }
}

/// Zapisuje przedział polecenia (jeśli to oczekiwanie, zgłoszenie, prezentacja albo trwało >= 150 µs).
pub fn record(name: &'static str, start: Instant, end: Instant) {
    let dur = end.duration_since(start);
    let interesting = dur >= Duration::from_micros(150) || name.starts_with("vkQueue") || name.starts_with("vkWait") || name.starts_with("vkAcquire") || name.contains("Semaphore");
    if !interesting || PHASE.load(Relaxed) != 2 {
        return;
    }
    let Some((t0, _)) = *T0.lock().unwrap_or_else(|e| e.into_inner()) else { return };
    if start < t0 {
        return;
    }
    let s = start.duration_since(t0).as_micros() as u64;
    let thread = NAME.with(|n| n.clone());
    let lw = LOCKW.with(|c| c.replace(0));
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).push((s, dur.as_micros() as u64, thread, name, lw));
}

fn finish() {
    if PHASE.swap(3, Relaxed) == 3 {
        return;
    }
    let mut ev = std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|e| e.into_inner()));
    ev.sort();
    eprintln!("[gpu-icd] OŚ CZASU ({} zdarzeń w oknie {} ms): początek µs, czas µs, wątek, polecenie, czekanie na blokadę µs", ev.len(), WINDOW.as_millis());
    for (s, d, t, n, lw) in ev {
        eprintln!("[tl] {s:>8} {d:>6} {t:<16} {n} lock={lw}");
    }
}
