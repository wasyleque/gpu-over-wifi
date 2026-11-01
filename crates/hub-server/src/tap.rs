//! Odczyt telemetrii InferSSD (`native-events.jsonl`) na żywo: skąd pochodził każdy użyty ekspert
//! (GPU / RAM / SSD), statystyki bieżącego tokena i całej odpowiedzi.
//!
//! Dane pochodzą z pliku zapisywanego przez zewnętrzny proces, więc są niezaufane:
//! uszkodzone linie i wartości spoza zakresu są ignorowane, nigdy nie powodują paniki.
#![allow(dead_code)]

use std::path::PathBuf;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

pub const TIER_NONE: u8 = 0;
pub const TIER_SSD: u8 = 1;
pub const TIER_RAM: u8 = 2;
pub const TIER_GPU: u8 = 3;

/// Maksymalna liczba ekspertów w jednej linii (ochrona przed złośliwie dużymi tablicami).
pub const MAX_EXPERTS_PER_LINE: usize = 64;
/// Przerwa (s) bez zdarzeń, po której kolejny token rozpoczyna nową odpowiedź.
pub const ANSWER_GAP_SECS: f64 = 5.0;
/// Czas (s) od ostatniego zdarzenia, w którym uznajemy, że generowanie trwa.
pub const RUNNING_WINDOW_SECS: f64 = 3.0;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TokenStats {
    pub token: u64,
    pub experts: u32,
    pub gpu: u32,
    pub ram: u32,
    pub ssd: u32,
    pub read_bytes: u64,
    pub wall_us: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AnswerStats {
    pub tokens: u32,
    pub experts: u64,
    pub gpu: u64,
    pub ram: u64,
    pub ssd: u64,
    pub read_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub layers: usize,
    pub experts: usize,
    /// Warstwa po warstwie, ekspert po ekspercie (`layer * experts + expert`): TIER_*.
    pub heat: Vec<u8>,
    /// Eksperci użyci w bieżącym tokenie: (warstwa, ekspert).
    pub picked: Vec<(u16, u16)>,
    /// Token w trakcie liczenia (może być niekompletny).
    pub current: TokenStats,
    /// Ostatni w pełni zakończony token.
    pub last: Option<TokenStats>,
    pub answer: AnswerStats,
    /// Tokeny odpowiedzi podzielone przez czas od pierwszego do ostatniego zdarzenia odpowiedzi.
    pub tokens_per_sec: f64,
    /// Czy w ostatnich `RUNNING_WINDOW_SECS` pojawiło się zdarzenie.
    pub running: bool,
}

#[derive(Debug)]
pub struct Tap {
    layers: usize,
    experts: usize,
    heat: Vec<u8>,
    current_token: Option<u64>,
    current: TokenStats,
    last: Option<TokenStats>,
    answer: AnswerStats,
    answer_start: Option<f64>,
    last_event_s: Option<f64>,
    picked: Vec<(u16, u16)>,
}

impl Tap {
    pub fn new(layers: usize, experts: usize) -> Self {
        let heat = vec![TIER_NONE; layers * experts];
        Tap {
            layers,
            experts,
            heat,
            current_token: None,
            current: TokenStats::default(),
            last: None,
            answer: AnswerStats::default(),
            answer_start: None,
            last_event_s: None,
            picked: Vec::new(),
        }
    }

    /// Przetwarza jedną linię JSON. `now_s` to znacznik czasu (sekundy) podany przez wywołującego.
    /// Uwzględnia tylko zdarzenia `ssd.place.layer` z `decode == true`.
    pub fn feed_line(&mut self, line: &str, now_s: f64) {
        let v = match serde_json::from_str::<serde_json::Value>(line).ok() {
            Some(v) => v,
            None => return,
        };

        // Sprawdź typ zdarzenia i czy decode jest true
        if v.get("ev").and_then(|v| v.as_str()) != Some("ssd.place.layer") {
            return;
        }
        
        if v.get("decode").and_then(|v| v.as_bool()) != Some(true) {
            return;
        }

        // Pobierz warstwę
        let layer = match v.get("layer").and_then(|v| v.as_u64()) {
            Some(layer) => layer,
            None => return,
        };
        
        if layer >= self.layers as u64 {
            return;
        }

        // Pobierz token
        let token = match v.get("token").and_then(|v| v.as_u64()) {
            Some(token) => token,
            None => return,
        };

        // Pobierz eksperci
        let selected_experts = match v.get("selected_experts").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return,
        };
        
        if selected_experts.len() > MAX_EXPERTS_PER_LINE {
            return;
        }

        // Sprawdź czy wszystkie eksperci są poprawne
        for expert_val in selected_experts {
            let expert = match expert_val.as_u64() {
                Some(expert) => expert,
                None => return,
            };
            
            if expert >= self.experts as u64 {
                return;
            }
        }

        // Sprawdź czy jest odpowiedni czas
        if let Some(last_event_s) = self.last_event_s {
            if now_s - last_event_s > ANSWER_GAP_SECS {
                self.answer = AnswerStats::default();
                self.answer_start = None;
            }
        }

        // Obsłuż przełączenie tokena
        let needs_new_token = match self.current_token {
            Some(current_token) => current_token != token,
            None => false,
        };

        if needs_new_token {
            self.last = Some(self.current);
            self.picked.clear();
            self.current = TokenStats { token, ..Default::default() };
            self.answer.tokens += 1;
        } else if self.current_token.is_none() {
            self.current = TokenStats { token, ..Default::default() };
            self.answer.tokens += 1;
        }

        // Ustaw flagę aktualnego tokena
        self.current_token = Some(token);

        // Pobierz tablice hitów
        let vram_hit = match v.get("vram_hit").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => &Vec::new(),
        };
        
        let cache_hit = match v.get("cache_hit").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => &Vec::new(),
        };

        // Pobierz dane o odczytach
        let read_bytes = v.get("read_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let wall_us = v.get("wall_us").and_then(|v| v.as_u64()).unwrap_or(0);

        // Dodaj eksperci do statystyk
        for (i, expert_val) in selected_experts.iter().enumerate() {
            let expert = expert_val.as_u64().unwrap_or(0) as u16;
            
            // Określ typ eksperta
            let tier = if i < vram_hit.len() && vram_hit[i].as_bool().unwrap_or(false) {
                TIER_GPU
            } else if i < cache_hit.len() && cache_hit[i].as_bool().unwrap_or(false) {
                TIER_RAM
            } else {
                TIER_SSD
            };
            
            // Zapisz w mapie cieplnej
            let heat_index = (layer as usize) * self.experts + (expert as usize);
            self.heat[heat_index] = tier;
            
            // Dodaj do statystyk tokena i odpowiedzi
            match tier {
                TIER_GPU => {
                    self.current.gpu += 1;
                    self.answer.gpu += 1;
                }
                TIER_RAM => {
                    self.current.ram += 1;
                    self.answer.ram += 1;
                }
                TIER_SSD => {
                    self.current.ssd += 1;
                    self.answer.ssd += 1;
                }
                _ => {}
            }
            
            self.current.experts += 1;
            self.answer.experts += 1;
            
            // Dodaj do listy wybranych eksperci
            self.picked.push((layer as u16, expert));
        }

        // Dodaj dane o odczytach i czasie
        self.current.read_bytes += read_bytes;
        self.answer.read_bytes += read_bytes;
        self.current.wall_us += wall_us;

        // Zapisz czas ostatniego zdarzenia
        self.last_event_s = Some(now_s);
        
        // Ustaw początek odpowiedzi jeśli jeszcze nie ustawiony
        if self.answer_start.is_none() {
            self.answer_start = Some(now_s);
        }
    }

    /// Zeruje mapę cieplną i statystyki ("Clear map").
    pub fn clear(&mut self) {
        self.heat.fill(TIER_NONE);
        self.current = TokenStats::default();
        self.last = None;
        self.answer = AnswerStats::default();
        self.answer_start = None;
        self.last_event_s = None;
        self.picked.clear();
        self.current_token = None;
    }

    pub fn snapshot(&self, now_s: f64) -> Snapshot {
        let heat_clone = self.heat.clone();
        let elapsed = match self.answer_start {
            Some(start) => now_s - start,
            None => 0.0,
        };
        
        let tokens_per_sec = if elapsed > 0.0 {
            self.answer.tokens as f64 / elapsed
        } else {
            0.0
        };
        
        let running = match self.last_event_s {
            Some(last) => now_s - last <= RUNNING_WINDOW_SECS,
            None => false,
        };

        Snapshot {
            layers: self.layers,
            experts: self.experts,
            heat: heat_clone,
            picked: self.picked.clone(),
            current: self.current,
            last: self.last,
            answer: self.answer,
            tokens_per_sec,
            running,
        }
    }
}

/// Czyta nowo dopisane linie pliku; radzi sobie z niedokończoną ostatnią linią i z obcięciem pliku.
#[derive(Debug)]
pub struct TailReader {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl TailReader {
    pub fn new(path: PathBuf) -> Self {
        TailReader {
            path,
            offset: 0,
            partial: String::new(),
        }
    }

    /// Zmienia ścieżkę (nowy przebieg InferSSD) i czyta od początku.
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = path;
        self.offset = 0;
        self.partial.clear();
    }

    /// Zwraca kompletne linie dopisane od ostatniego wywołania. Brak pliku to pusta lista.
    pub fn read_new_lines(&mut self) -> Vec<String> {
        let mut result = Vec::new();
        
        // Spróbuj otworzyć plik
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return result,
        };
        
        // Pobierz rozmiar pliku
        let len = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => return result,
        };
        
        // Jeśli plik jest krótszy niż offset, zresetuj offset i częściowy bufor
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        
        // Przejdź do odpowiedniego miejsca w pliku
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return result,
        };
        
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return result;
        }
        
        // Odczytaj resztę pliku
        let mut buffer = Vec::new();
        match file.read_to_end(&mut buffer) {
            Ok(_) => {},
            Err(_) => return result,
        };
        
        self.offset += buffer.len() as u64;
        
        // Dodaj dane do częściowego bufora
        let content = String::from_utf8_lossy(&buffer);
        self.partial.push_str(&content);
        
        // Kompletne linie (zakończone '\n') wracają do wywołującego, reszta czeka w `partial`.
        while let Some(pos) = self.partial.find('\n') {
            let mut line: String = self.partial.drain(..=pos).collect();
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            result.push(line);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn line(layer: u32, token: u64, experts: &[u32], vram: &[bool], cache: &[bool], read_bytes: u64) -> String {
        format!(
            r#"{{"v":1,"ev":"ssd.place.layer","decode":true,"layer":{layer},"token":{token},"selected_experts":{experts:?},"vram_hit":{vram:?},"cache_hit":{cache:?},"read_bytes":{read_bytes},"wall_us":1000}}"#
        )
    }

    #[test]
    fn single_layer_sets_tiers_and_picks() {
        let mut t = Tap::new(4, 8);
        t.feed_line(&line(3, 0, &[1, 2, 3], &[true, false, false], &[false, true, false], 100), 0.0);
        let s = t.snapshot(0.1);
        assert_eq!((s.layers, s.experts), (4, 8));
        assert_eq!(s.heat[3 * 8 + 1], TIER_GPU);
        assert_eq!(s.heat[3 * 8 + 2], TIER_RAM);
        assert_eq!(s.heat[3 * 8 + 3], TIER_SSD);
        assert_eq!(s.heat.iter().filter(|&&h| h != TIER_NONE).count(), 3);
        let mut p = s.picked.clone();
        p.sort();
        assert_eq!(p, vec![(3, 1), (3, 2), (3, 3)]);
        assert_eq!((s.current.experts, s.current.gpu, s.current.ram, s.current.ssd), (3, 1, 1, 1));
        assert_eq!(s.current.read_bytes, 100);
    }

    #[test]
    fn vram_hit_wins_over_cache_hit() {
        let mut t = Tap::new(2, 4);
        t.feed_line(&line(0, 0, &[0], &[true], &[true], 0), 0.0);
        assert_eq!(t.snapshot(0.0).heat[0], TIER_GPU);
    }

    #[test]
    fn new_token_finishes_previous_and_clears_picks_but_keeps_heat() {
        let mut t = Tap::new(2, 8);
        t.feed_line(&line(0, 0, &[1, 2], &[true, false], &[false, false], 10), 0.0);
        t.feed_line(&line(1, 0, &[3], &[false], &[true], 20), 0.0);
        t.feed_line(&line(0, 1, &[5], &[false], &[false], 30), 0.1);
        let s = t.snapshot(0.1);
        let last = s.last.expect("token 0 zakończony");
        assert_eq!((last.token, last.experts, last.gpu, last.ram, last.ssd, last.read_bytes), (0, 3, 1, 1, 1, 30));
        assert_eq!((s.current.token, s.current.experts, s.current.ssd), (1, 1, 1));
        assert_eq!(s.picked, vec![(0, 5)], "tylko eksperci bieżącego tokena");
        assert_eq!(s.heat[1], TIER_GPU, "mapa pamięta poprzednie tokeny");
        assert_eq!(s.heat[8 + 3], TIER_RAM);
    }

    #[test]
    fn answer_totals_accumulate_over_tokens() {
        let mut t = Tap::new(1, 8);
        for tok in 0..3u64 {
            t.feed_line(&line(0, tok, &[0, 1], &[true, false], &[false, true], 100), tok as f64 * 0.5);
        }
        let s = t.snapshot(1.0);
        // Tokeny 0 i 1 są zakończone, token 2 jeszcze trwa; odpowiedź liczy też trwający.
        assert_eq!(s.answer.tokens, 3);
        assert_eq!((s.answer.experts, s.answer.gpu, s.answer.ram, s.answer.ssd), (6, 3, 3, 0));
        assert_eq!(s.answer.read_bytes, 300);
    }

    #[test]
    fn tokens_per_second_uses_event_timespan() {
        let mut t = Tap::new(1, 4);
        for tok in 0..5u64 {
            t.feed_line(&line(0, tok, &[0], &[true], &[false], 0), tok as f64 * 0.25);
        }
        let s = t.snapshot(1.0);
        assert!((s.tokens_per_sec - 5.0).abs() < 1e-9, "5 tokenów w 1,0 s, jest {}", s.tokens_per_sec);
    }

    #[test]
    fn running_flag_follows_recent_events() {
        let mut t = Tap::new(1, 4);
        assert!(!t.snapshot(0.0).running);
        t.feed_line(&line(0, 0, &[0], &[true], &[false], 0), 10.0);
        assert!(t.snapshot(11.0).running);
        assert!(!t.snapshot(10.0 + RUNNING_WINDOW_SECS + 0.5).running);
    }

    #[test]
    fn long_gap_starts_a_new_answer_but_heat_stays() {
        let mut t = Tap::new(1, 4);
        t.feed_line(&line(0, 0, &[0], &[true], &[false], 5), 0.0);
        t.feed_line(&line(0, 1, &[1], &[false], &[false], 5), 0.1);
        t.feed_line(&line(0, 0, &[2], &[false], &[true], 7), 0.1 + ANSWER_GAP_SECS + 1.0);
        let s = t.snapshot(6.5);
        assert_eq!(s.answer.tokens, 1, "nowa odpowiedź zaczyna od zera");
        assert_eq!(s.answer.read_bytes, 7);
        assert_eq!(s.heat[0], TIER_GPU);
        assert_eq!(s.heat[2], TIER_RAM);
    }

    #[test]
    fn clear_resets_heat_and_stats() {
        let mut t = Tap::new(1, 4);
        t.feed_line(&line(0, 0, &[0, 1], &[true, true], &[false, false], 5), 0.0);
        t.clear();
        let s = t.snapshot(0.0);
        assert!(s.heat.iter().all(|&h| h == TIER_NONE));
        assert_eq!(s.answer, AnswerStats::default());
        assert!(s.picked.is_empty());
        assert_eq!(s.last, None);
    }

    #[test]
    fn garbage_and_out_of_range_input_never_panics_and_is_ignored() {
        let mut t = Tap::new(2, 4);
        let huge: Vec<u32> = (0..100_000).collect();
        let bad = [
            "".to_string(),
            "not json".to_string(),
            "{".to_string(),
            r#"{"ev":"ssd.io.read","decode":true,"layer":0}"#.to_string(),
            r#"{"ev":"ssd.place.layer","decode":false,"layer":0,"token":0,"selected_experts":[1],"vram_hit":[true],"cache_hit":[false]}"#.to_string(),
            r#"{"ev":"ssd.place.layer","decode":true,"layer":999,"token":0,"selected_experts":[1],"vram_hit":[true],"cache_hit":[false]}"#.to_string(),
            r#"{"ev":"ssd.place.layer","decode":true,"layer":0,"token":0,"selected_experts":[99999],"vram_hit":[true],"cache_hit":[false]}"#.to_string(),
            r#"{"ev":"ssd.place.layer","decode":true,"layer":-1,"token":0,"selected_experts":[1]}"#.to_string(),
            r#"{"ev":"ssd.place.layer","decode":true,"layer":0,"token":"x","selected_experts":"y"}"#.to_string(),
            line(0, 0, &huge, &vec![true; 100_000], &vec![false; 100_000], 1),
        ];
        for b in &bad {
            t.feed_line(b, 0.0);
        }
        let s = t.snapshot(0.0);
        assert!(s.heat.iter().all(|&h| h == TIER_NONE), "żadna zła linia nie może zmienić mapy");
        assert!(s.picked.is_empty());
    }

    #[test]
    fn missing_hit_arrays_default_to_ssd() {
        let mut t = Tap::new(1, 4);
        t.feed_line(r#"{"ev":"ssd.place.layer","decode":true,"layer":0,"token":0,"selected_experts":[2],"read_bytes":9}"#, 0.0);
        assert_eq!(t.snapshot(0.0).heat[2], TIER_SSD);
    }

    fn temp_file(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tap-{tag}-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn tail_reader_returns_only_complete_new_lines() {
        let p = temp_file("tail");
        let mut r = TailReader::new(p.clone());
        assert!(r.read_new_lines().is_empty(), "brak pliku = pusta lista");
        let mut f = std::fs::File::create(&p).unwrap();
        write!(f, "aaa\nbbb\ncc").unwrap();
        f.flush().unwrap();
        assert_eq!(r.read_new_lines(), vec!["aaa", "bbb"]);
        assert!(r.read_new_lines().is_empty());
        write!(f, "c\nddd\n").unwrap();
        f.flush().unwrap();
        assert_eq!(r.read_new_lines(), vec!["ccc", "ddd"], "dokończona linia wraca w całości");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn tail_reader_handles_truncation_and_path_change() {
        let p = temp_file("trunc");
        std::fs::write(&p, "one\ntwo\n").unwrap();
        let mut r = TailReader::new(p.clone());
        assert_eq!(r.read_new_lines(), vec!["one", "two"]);
        std::fs::write(&p, "x\n").unwrap(); // plik krótszy niż offset => czytaj od początku
        assert_eq!(r.read_new_lines(), vec!["x"]);
        let q = temp_file("other");
        std::fs::write(&q, "q1\n").unwrap();
        r.set_path(q.clone());
        assert_eq!(r.read_new_lines(), vec!["q1"]);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&q);
    }
}
