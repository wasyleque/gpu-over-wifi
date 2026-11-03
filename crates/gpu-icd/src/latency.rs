//! Opóźnienie klatki od oddania jej przez grę (`vkQueuePresentKHR`) do wyświetlenia lokalnie: od niego zależy, jak "żywo" reaguje
//! kursor rysowany przez grę. Mierzone na tym samym zegarze po obu końcach (oba w procesie klienta). Diagnostyka, `GPU_ICD_STATS`.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Ile oddanych, a jeszcze nie wyświetlonych klatek pamiętamy (starsze są pomijane jako zgubione).
const PENDING_MAX: usize = 512;
/// Górne granice przedziałów rozkładu opóźnienia, w ms (ostatni przedział: powyżej).
const BUCKETS_MS: [u64; 5] = [20, 40, 70, 120, 250];

#[derive(Default)]
struct State {
    pending: VecDeque<(u32, Instant)>,
    sum_us: u64,
    max_us: u64,
    n: u64,
    hist: [u64; 6],
    /// Ile razy `wait_for_room` musiał czekać i ile łącznie to trwało (µs): koszt hamowania gry.
    waits: u64,
    waited_us: u64,
    /// Kolejne przekroczenia czasu oczekiwania: gdy klatki w ogóle nie są wyświetlane (brak wideo), hamowanie się wyłącza.
    timeouts_in_row: u32,
    throttle_off: bool,
}

static S: Mutex<Option<State>> = Mutex::new(None);
static ROOM: Condvar = Condvar::new();

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

/// Klatka `seq` została oddana do wysłania.
pub fn sent(seq: u32) {
    with(|s| {
        if s.pending.len() >= PENDING_MAX {
            s.pending.pop_front();
        }
        s.pending.push_back((seq, Instant::now()));
    });
}

/// Klatka `seq` została wyświetlona; klatki starsze, które nigdy nie dotarły, są pomijane.
pub fn shown(seq: u32) {
    with(|s| {
        while let Some(&(q, _)) = s.pending.front() {
            if q.wrapping_sub(seq) as i32 >= 0 {
                break; // q >= seq (z obsługą zawinięcia numeru)
            }
            s.pending.pop_front();
        }
        if let Some(&(q, t)) = s.pending.front() {
            if q == seq {
                s.pending.pop_front();
                s.record(t.elapsed());
                s.timeouts_in_row = 0;
            }
        }
    });
    ROOM.notify_all();
}

/// Ogranicza liczbę klatek oddanych, a jeszcze niewyświetlonych, do `max`: bez tego gra (która nie czeka na wyświetlenie)
/// produkuje klatki szybciej, niż potok je dostarcza, i opóźnienie rośnie do sekund. Czeka najwyżej `limit` na klatkę;
/// po 5 kolejnych przekroczeniach (klatki nie są wyświetlane wcale) hamowanie się wyłącza.
pub fn wait_for_room(max: usize, limit: Duration) {
    let start = Instant::now();
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    let mut waited = false;
    loop {
        let st = g.get_or_insert_with(State::default);
        if st.throttle_off || st.pending.len() < max {
            break;
        }
        let left = limit.saturating_sub(start.elapsed());
        if left.is_zero() {
            st.timeouts_in_row += 1;
            st.throttle_off = st.timeouts_in_row >= 5;
            break;
        }
        waited = true;
        g = ROOM.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    if waited {
        if let Some(st) = g.as_mut() {
            st.waits += 1;
            st.waited_us += start.elapsed().as_micros() as u64;
        }
    }
}

impl State {
    fn record(&mut self, d: Duration) {
        let us = d.as_micros() as u64;
        self.sum_us += us;
        self.max_us = self.max_us.max(us);
        self.n += 1;
        let ms = us / 1000;
        let i = BUCKETS_MS.iter().position(|&b| ms < b).unwrap_or(BUCKETS_MS.len());
        self.hist[i] += 1;
    }
}

/// Podsumowanie od poprzedniego wywołania (i zerowanie); `None`, gdy w tym czasie nie wyświetlono żadnej klatki.
pub fn report_and_reset() -> Option<String> {
    with(|s| {
        if s.n == 0 {
            return None;
        }
        let pct = |c: u64| c * 100 / s.n;
        let mut out = format!(
            "opóźnienie klatki (oddanie→wyświetlenie): {} klatek, średnio {:.0} ms, maks {:.0} ms; <20 ms {}%, <40 {}%, <70 {}%, <120 {}%, <250 {}%, więcej {}%",
            s.n,
            s.sum_us as f64 / s.n as f64 / 1000.0,
            s.max_us as f64 / 1000.0,
            pct(s.hist[0]),
            pct(s.hist[1]),
            pct(s.hist[2]),
            pct(s.hist[3]),
            pct(s.hist[4]),
            pct(s.hist[5])
        );
        out.push_str(&format!("; hamowanie gry: {} razy, łącznie {:.0} ms", s.waits, s.waited_us as f64 / 1000.0));
        let pending = std::mem::take(&mut s.pending);
        *s = State { pending, timeouts_in_row: s.timeouts_in_row, throttle_off: s.throttle_off, ..State::default() };
        Some(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Testy współdzielą stan globalny, więc nie mogą biec równolegle.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn measures_delay_skips_lost_frames_and_resets_between_reports() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        with(|s| *s = State::default());
        sent(10);
        sent(11);
        sent(12);
        std::thread::sleep(Duration::from_millis(30));
        shown(12); // 10 i 11 zgubione: pomijane bez wpływu na wynik
        let r = report_and_reset().expect("jedna klatka wyświetlona");
        assert!(r.starts_with("opóźnienie klatki (oddanie→wyświetlenie): 1 klatek"), "{r}");
        assert!(report_and_reset().is_none(), "po zerowaniu brak danych");
        shown(99); // nieznana klatka nic nie zmienia
        assert!(report_and_reset().is_none());
    }
    #[test]
    fn throttling_waits_for_a_shown_frame_and_gives_up_when_nothing_is_shown() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        with(|s| *s = State::default());
        // Jest miejsce: wraca od razu.
        let t = Instant::now();
        wait_for_room(2, Duration::from_millis(500));
        assert!(t.elapsed() < Duration::from_millis(100));
        sent(1);
        sent(2);
        // Brak miejsca: czeka na wyświetlenie klatki 1 w drugim wątku.
        let h = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(60));
            shown(1);
        });
        let t = Instant::now();
        wait_for_room(2, Duration::from_millis(2000));
        let waited = t.elapsed();
        h.join().unwrap();
        assert!(waited >= Duration::from_millis(40) && waited < Duration::from_millis(1500), "{waited:?}");
        // Nic nie jest wyświetlane: po limicie wraca, a po 5 razach wyłącza hamowanie.
        sent(3);
        for _ in 0..5 {
            wait_for_room(2, Duration::from_millis(5));
        }
        let t = Instant::now();
        wait_for_room(2, Duration::from_millis(500));
        assert!(t.elapsed() < Duration::from_millis(50), "hamowanie powinno być wyłączone");
        with(|s| *s = State::default());
    }

}
