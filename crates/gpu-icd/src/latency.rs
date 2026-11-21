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
    /// Odstępy między kolejnymi wyświetleniami (µs) w bieżącym oknie raportu i moment poprzedniego wyświetlenia: równomierność klatek (płynność).
    last_shown: Option<Instant>,
    ivals: Vec<u32>,
    /// To samo dla oddawania klatek przez grę (vkQueuePresentKHR): porównanie rozdziela nierówność wytwarzania od nierówności dostarczania.
    last_sent: Option<Instant>,
    sivals: Vec<u32>,
}

static S: Mutex<Option<State>> = Mutex::new(None);
static ROOM: Condvar = Condvar::new();

/// Pacing oddawania klatek (`GPU_ICD_PACE=1`, współczynnik `GPU_ICD_PACE_F`, domyślnie 0,9): early-return w `vkWaitSemaphores` pozwala grze wyprzedzić
/// serwer, a potem gra staje na synchronizacji, więc klatki wychodzą seriami (p95 odstępu 85-115 ms przy medianie 25-30 ms). Klatka, która przychodzi
/// wyraźnie szybciej niż średni surowy odstęp, jest wstrzymywana do tego odstępu (przed blokadą instancji), więc gra mierzy równe czasy klatek.
struct Pace {
    last: Option<Instant>,
    slept_us: f64,
    ewma_us: f64,
}

static PACE: Mutex<Pace> = Mutex::new(Pace { last: None, slept_us: 0.0, ewma_us: 0.0 });
/// Twardy limit klatek (`GPU_ICD_PACE_FPS=30`): następny termin oddania klatki liczony bezwzględnie, bez nadrabiania po opóźnieniu.
static CAP_NEXT: Mutex<Option<Instant>> = Mutex::new(None);

fn cap_interval() -> Option<Duration> {
    static T: std::sync::OnceLock<Option<Duration>> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        let fps: f64 = std::env::var("GPU_ICD_PACE_FPS").ok()?.parse().ok()?;
        (fps >= 5.0 && fps <= 240.0).then(|| Duration::from_micros((1e6 / fps) as u64))
    })
}
static PACE_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PACE_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn pace_factor() -> Option<f64> {
    static F: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        std::env::var_os("GPU_ICD_PACE")?;
        Some(std::env::var("GPU_ICD_PACE_F").ok().and_then(|v| v.parse().ok()).filter(|f| (0.3..=1.2).contains(f)).unwrap_or(0.9))
    })
}

/// Wołać przy wejściu w `vkQueuePresentKHR`, ZANIM weźmiemy blokadę instancji (usypia wątek oddający klatkę).
pub fn pace() {
    crate::tuner::on_present();
    crate::timeline::on_present();
    if let Some(t) = cap_interval() {
        let wake = {
            let mut n = CAP_NEXT.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let deadline = n.unwrap_or(now);
            // Opóźniona klatka nie powoduje nadrabiania: następny termin liczymy od teraz.
            *n = Some(deadline.max(now) + t);
            deadline
        };
        let now = Instant::now();
        if wake > now {
            let d = wake - now;
            std::thread::sleep(d);
            PACE_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            PACE_US.fetch_add(d.as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        return;
    }
    let Some(factor) = pace_factor() else { return };
    let sleep_us = {
        let mut p = PACE.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let mut sleep = 0.0;
        if let Some(last) = p.last {
            // Surowy odstęp gry: czas między wejściami minus to, co sami przespaliśmy poprzednio.
            let raw = (now.duration_since(last).as_micros() as f64 - p.slept_us).max(0.0);
            p.ewma_us = if p.ewma_us == 0.0 { raw } else { 0.9 * p.ewma_us + 0.1 * raw.min(4.0 * p.ewma_us) };
            let target = p.ewma_us * factor;
            if raw < target {
                sleep = target - raw;
            }
        }
        p.slept_us = sleep;
        p.last = Some(now);
        sleep
    };
    if sleep_us >= 100.0 {
        std::thread::sleep(Duration::from_micros(sleep_us as u64));
        PACE_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        PACE_US.fetch_add(sleep_us as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

/// Klatka `seq` została oddana do wysłania.
pub fn sent(seq: u32) {
    with(|s| {
        let now = Instant::now();
        if let Some(p) = s.last_sent.replace(now) {
            if s.sivals.len() < 4096 {
                s.sivals.push(now.duration_since(p).as_micros().min(u32::MAX as u128) as u32);
            }
        }
        if s.pending.len() >= PENDING_MAX {
            s.pending.pop_front();
        }
        s.pending.push_back((seq, Instant::now()));
    });
}

/// Klatka `seq` została wyświetlona; klatki starsze, które nigdy nie dotarły, są pomijane.
pub fn shown(seq: u32) {
    with(|s| {
        let now = Instant::now();
        if let Some(p) = s.last_shown.replace(now) {
            if s.ivals.len() < 4096 {
                s.ivals.push(now.duration_since(p).as_micros().min(u32::MAX as u128) as u32);
            }
        }
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
        // Klatki znów się wyświetlają: hamowanie, wyłączone na stałe po 5 przekroczeniach czasu (np. przy starcie, zanim ruszyło wideo), wraca. Bez tego do końca sesji
        // nic nie ograniczało klatek w locie (Rise of the Tomb Raider z wyłączonym vsync: opóźnienie 4-7 s i "lepkie" menu, `hamowanie gry: 0 razy`).
        s.throttle_off = false;
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

fn spread(v: &mut Vec<u32>, label: &str) -> Option<String> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let q = |p: usize| v[(v.len() * p / 100).min(v.len() - 1)] as f64 / 1000.0;
    let med = q(50);
    let over2 = v.iter().filter(|&&x| x as f64 / 1000.0 > 2.0 * med).count();
    let over100 = v.iter().filter(|&&x| x > 100_000).count();
    let r = format!("{label}: p50 {:.1} ms, p95 {:.1}, p99 {:.1}, maks {:.1}; ponad 2×p50: {}, ponad 100 ms: {} z {}", med, q(95), q(99), *v.last().unwrap() as f64 / 1000.0, over2, over100, v.len());
    v.clear();
    Some(r)
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
        if let Some(t) = spread(&mut s.ivals, "odstępy wyświetlenia") {
            out.push_str(&format!("; {t}"));
        }
        if let Some(t) = spread(&mut s.sivals, "odstępy oddania") {
            out.push_str(&format!("; {t}"));
        }
        if pace_factor().is_some() || cap_interval().is_some() {
            use std::sync::atomic::Ordering::Relaxed;
            out.push_str(&format!("; pacing: {} wstrzymań, łącznie {} ms", PACE_N.swap(0, Relaxed), PACE_US.swap(0, Relaxed) / 1000));
        }
        out.push_str(&format!("; hamowanie gry: {} razy, łącznie {:.0} ms", s.waits, s.waited_us as f64 / 1000.0));
        let pending = std::mem::take(&mut s.pending);
        *s = State { pending, timeouts_in_row: s.timeouts_in_row, throttle_off: s.throttle_off, last_shown: s.last_shown, last_sent: s.last_sent, ..State::default() };
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
        // Wyświetlenie klatki włącza hamowanie z powrotem.
        shown(3);
        sent(4);
        sent(5);
        let t = Instant::now();
        wait_for_room(2, Duration::from_millis(80));
        assert!(t.elapsed() >= Duration::from_millis(60), "po wyświetleniu klatki hamowanie znowu czeka na miejsce: {:?}", t.elapsed());
        with(|s| *s = State::default());
    }

}
