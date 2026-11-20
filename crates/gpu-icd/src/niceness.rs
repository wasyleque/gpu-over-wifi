//! Obniżanie priorytetu wątków gry, które tylko kręcą się w pętli oczekiwania (`GPU_ICD_NICE_SPINNERS=<nice>`): profil CPU Cyberpunka pokazał, że 11 wątków
//! `redDispatcher` zużywa ok. 70% próbek w jednej ciasnej pętli aktywnego czekania, więc nasze wątki pomocnicze (nadawca, odbiornik, obsługa błędów stron) mogą
//! czekać na procesor. Podniesienie wartości `nice` jest dozwolone bez uprawnień (obniżenie nie), więc zmieniamy tylko wątki gry, nie nasze.

extern "C" {
    fn setpriority(which: i32, who: u32, prio: i32) -> i32;
}

const PRIO_PROCESS: i32 = 0;

/// Uruchamia wątek, który co 2 s ustawia `nice` wątkom procesu o nazwie zaczynającej się od `prefix` (nowe wątki też).
pub fn start(prefix: &'static str, nice: i32) {
    let _ = std::thread::Builder::new().name("gpu-icd-nice".into()).spawn(move || {
        let mut done = std::collections::HashSet::new();
        loop {
            if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
                for e in dir.flatten() {
                    let Some(tid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
                    if done.contains(&tid) {
                        continue;
                    }
                    let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) else { continue };
                    if comm.starts_with(prefix) {
                        // SAFETY: wywołanie systemowe na identyfikatorze wątku własnego procesu.
                        unsafe { setpriority(PRIO_PROCESS, tid, nice) };
                        done.insert(tid);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

/// Uruchamia obniżanie priorytetu wg `GPU_ICD_NICE_SPINNERS` (wartość 1..19; brak = nic nie robi).
pub fn start_from_env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(n) = std::env::var("GPU_ICD_NICE_SPINNERS").ok().and_then(|v| v.parse::<i32>().ok()).filter(|n| (1..=19).contains(n)) {
            start("redDispatcher", n);
        }
    });
}
