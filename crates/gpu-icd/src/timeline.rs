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
static EVENTS: Mutex<Vec<(u64, u64, String, &'static str, u64, u64, u64)>> = Mutex::new(Vec::new());

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
    /// Czas oczekiwania na odpowiedź serwera (µs) i ponownego wzięcia blokady po odpowiedzi (µs) w bieżącym poleceniu.
    pub static RECVW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub static RELOCKW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static NAME: String = std::fs::read_to_string("/proc/thread-self/comm").map(|s| s.trim().to_string()).unwrap_or_default();
}

/// Tryb skoków (`GPU_ICD_SPIKES=<ms>`): stały bufor ostatnich zdarzeń; gdy odstęp między kolejnymi `vkQueuePresentKHR` przekroczy próg, do logu trafia lista
/// zdarzeń z tego odstępu (które wywołania, które wątki, ile trwały i ile z tego to czekanie na blokadę instancji). Do 14 zrzutów na sesję.
fn spike_ms() -> Option<u64> {
    static V: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("GPU_ICD_SPIKES").ok().and_then(|v| v.parse().ok()).filter(|m| *m >= 10))
}

type Ev = (Instant, u64, String, &'static str, u64);
static RING: Mutex<std::collections::VecDeque<Ev>> = Mutex::new(std::collections::VecDeque::new());
static LAST_PRESENT: Mutex<Option<Instant>> = Mutex::new(None);
static DUMPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const RING_MAX: usize = 6000;

/// Zapis zdarzenia do bufora skoków (czas trwania w µs, nazwa polecenia lub rodzaju zdarzenia).
pub fn span(name: &'static str, start: Instant, end: Instant) {
    if spike_ms().is_none() {
        return;
    }
    let lw = LOCKW.with(|c| c.replace(0));
    let thread = NAME.with(|n| n.clone());
    let mut r = RING.lock().unwrap_or_else(|e| e.into_inner());
    if r.len() >= RING_MAX {
        r.pop_front();
    }
    r.push_back((start, end.duration_since(start).as_micros() as u64, thread, name, lw));
}

/// Wołane przy każdym `vkQueuePresentKHR`: przy odstępie ponad progiem zrzuca zdarzenia z tego odstępu.
pub fn on_present() {
    let Some(thr) = spike_ms() else { return };
    let now = Instant::now();
    let prev = LAST_PRESENT.lock().unwrap_or_else(|e| e.into_inner()).replace(now);
    let Some(prev) = prev else { return };
    let gap = now.duration_since(prev);
    if gap.as_millis() as u64 <= thr || DUMPS.load(std::sync::atomic::Ordering::Relaxed) >= 14 {
        return;
    }
    DUMPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let r = RING.lock().unwrap_or_else(|e| e.into_inner());
    let mut ev: Vec<&Ev> = r.iter().filter(|e| e.0 + Duration::from_micros(e.1) >= prev && e.0 <= now).collect();
    ev.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("[spike] odstęp między klatkami {} ms; {} zdarzeń w odstępie, najdłuższe:", gap.as_millis(), ev.len());
    for e in ev.iter().take(14) {
        let off = e.0.checked_duration_since(prev).map(|d| d.as_micros() as i64).unwrap_or(-(prev.duration_since(e.0).as_micros() as i64));
        eprintln!("[spike]   +{:>7} µs  {:>7} µs  {:<16} {}  lock={}", off, e.1, e.2, e.3, e.4);
    }
}

/// Czy nagrywamy teraz (wołane przed każdym poleceniem).
pub fn recording() -> bool {
    init();
    if spike_ms().is_some() {
        return true;
    }
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
    span(name, start, end);
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
    let (rv, rl) = (RECVW.with(|c| c.replace(0)), RELOCKW.with(|c| c.replace(0)));
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).push((s, dur.as_micros() as u64, thread, name, lw, rv, rl));
}

fn finish() {
    if PHASE.swap(3, Relaxed) == 3 {
        return;
    }
    let mut ev = std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|e| e.into_inner()));
    ev.sort();
    eprintln!("[gpu-icd] OŚ CZASU ({} zdarzeń w oknie {} ms): początek µs, czas µs, wątek, polecenie, czekanie na blokadę µs", ev.len(), WINDOW.as_millis());
    for (s, d, t, n, lw, rv, rl) in ev {
        eprintln!("[tl] {s:>8} {d:>6} {t:<16} {n} lock={lw} recv={rv} relock={rl}");
    }
}

/// Czas zablokowania wątków w poleceniach z obiegiem po sieci, wg (wątek, polecenie): (liczba, suma µs). Diagnostyka ścieżki krytycznej
/// (`GPU_ICD_STATS=periodic`): pokazuje, KTÓRY wątek czeka i na co, czego nie widać w zbiorczym "obiegi wg poleceń".
static BLOCKED: Mutex<Option<std::collections::HashMap<(String, &'static str), (u64, u64)>>> = Mutex::new(None);

pub fn blocked(cmd: &'static str, us: u64) {
    let name = NAME.with(|n| n.clone());
    let mut g = BLOCKED.lock().unwrap_or_else(|e| e.into_inner());
    let e = g.get_or_insert_with(Default::default).entry((name, cmd)).or_insert((0, 0));
    e.0 += 1;
    e.1 += us;
}

/// Pierwsze `n` pozycji wg sumarycznego czasu: "wątek/polecenie liczba x czas ms".
pub fn blocked_report(n: usize) -> String {
    let g = BLOCKED.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = g.as_ref() else { return String::new() };
    let mut v: Vec<_> = m.iter().collect();
    v.sort_by_key(|(_, (_, us))| std::cmp::Reverse(*us));
    v.iter().take(n).map(|((t, c), (k, us))| format!("{t}/{} {k}x {}ms", c.trim_start_matches("vk"), us / 1000)).collect::<Vec<_>>().join("; ")
}
