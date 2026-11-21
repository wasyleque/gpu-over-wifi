//! Skan zapisów aplikacji poza blokadą instancji. DOMYŚLNIE WYŁĄCZONY (`GPU_ICD_PRESCAN=1` włącza): pomiar 2026-11-21 nie wykazał zysku
//! (25,00 FPS z, 25,70 bez; czas skanu pod blokadą spadł z 82,8 do 43,8 s, a FPS bez zmian).
//!
//! Blokujące oczekiwanie (`vkWaitSemaphores`, `vkWaitForFences`) po zgłoszeniu pracy musi wysłać na serwer zapisy aplikacji do odwzorowanej pamięci.
//! Skan areny (`PAGEMAP_SCAN`, ok. 0,9 ms) szedł pod globalną blokadą instancji i zatrzymywał wszystkie inne wątki (serial 80 tys. oczekiwań).
//! Tutaj wątek oczekujący skanuje PRZED wzięciem blokady, a pod blokadą tylko zużywa gotowe zakresy.
//!
//! Poprawność: zapis aplikacji poprzedza w czasie zgłoszenie, które go używa, a to poprzedza oczekiwanie. Numer `SUBMIT_SEQ` rośnie przy każdym
//! zgłoszeniu (pod blokadą, przed zwróceniem sterowania aplikacji). Skan zapamiętuje numer sprzed swojego początku; pod blokadą zakresy wolno uznać za
//! kompletne tylko wtedy, gdy numer się nie zmienił, czyli żadne zgłoszenie nie zakończyło się w trakcie skanu. W przeciwnym razie robimy skan
//! pełny pod blokadą, jak dotąd. Zakresy zabrane jądru (bity zapisu są wtedy zerowane) nigdy nie giną: każdy skan i każde zużycie przechodzi przez
//! ten sam mutex, a zużycie bierze też zaległe zakresy.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

pub static SUBMIT_SEQ: AtomicU64 = AtomicU64::new(0);
pub static SCAN_PENDING: AtomicBool = AtomicBool::new(false);
pub static HITS: AtomicU64 = AtomicU64::new(0);
pub static MISSES: AtomicU64 = AtomicU64::new(0);
pub static COALESCED: AtomicU64 = AtomicU64::new(0);
pub static SCAN_NS: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Acc {
    /// Numer zgłoszeń sprzed ostatniego skanu zewnętrznego (`None`: brak ważnego skanu).
    gen: Option<u64>,
    ranges: Vec<(usize, usize)>,
}

static ACC: Mutex<Acc> = Mutex::new(Acc { gen: None, ranges: Vec::new() });

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("GPU_ICD_PRESCAN").map(|v| v == "1").unwrap_or(false))
}

/// Wołane przed blokadą instancji przez wątek, który zaraz zablokuje się na oczekiwaniu.
pub fn before_wait() {
    if !enabled() || !SCAN_PENDING.load(Relaxed) {
        return;
    }
    let mut g = ACC.lock().unwrap_or_else(|e| e.into_inner());
    let seq = SUBMIT_SEQ.load(Relaxed);
    if g.gen == Some(seq) {
        COALESCED.fetch_add(1, Relaxed);
        return;
    }
    let Some(arena) = crate::memtrack::arena() else { return };
    let t = std::time::Instant::now();
    if let Some(mut r) = arena.take_dirty_all() {
        g.ranges.append(&mut r);
        merge(&mut g.ranges);
        g.gen = Some(seq);
    }
    SCAN_NS.fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
}

/// Zużycie pod blokadą. `allow_fast`: wolno pominąć skan, jeśli zakresy są kompletne (tylko ścieżka oczekiwania).
/// Zwraca zakresy do wysłania (zaległe z poprzednich skanów zawsze wchodzą) oraz `true`, gdy pełny skan jest zbędny.
pub fn take(allow_fast: bool) -> (Vec<(usize, usize)>, bool) {
    let mut g = ACC.lock().unwrap_or_else(|e| e.into_inner());
    let complete = allow_fast && g.gen == Some(SUBMIT_SEQ.load(Relaxed));
    if allow_fast {
        if complete { HITS.fetch_add(1, Relaxed); } else { MISSES.fetch_add(1, Relaxed); }
    }
    (std::mem::take(&mut g.ranges), complete)
}

/// Są zaległe zakresy (zabrane jądru, jeszcze nie wysłane).
pub fn has_backlog() -> bool {
    !ACC.lock().unwrap_or_else(|e| e.into_inner()).ranges.is_empty()
}

pub fn merge(v: &mut Vec<(usize, usize)>) {
    v.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(v.len());
    for &(a, b) in v.iter() {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    *v = out;
}

#[cfg(test)]
mod tests {
    use super::merge;
    #[test]
    fn merge_sorts_and_joins() {
        let mut v = vec![(10, 20), (0, 5), (20, 30), (4, 6), (40, 50)];
        merge(&mut v);
        assert_eq!(v, vec![(0, 6), (10, 30), (40, 50)]);
    }
}
