//! Śledzenie nieaktualnych stron lokalnej kopii pamięci odwzorowanej przez aplikację (M3: odczyt leniwy).
//! Po zakończeniu pracy GPU strony potencjalnie zapisane przez serwer oznaczamy jako nieaktualne i pobieramy
//! je dopiero przy błędzie strony (w oknach wyrównanych do zadanej potęgi dwójki).

#![allow(dead_code)]

pub const PAGE: usize = 4096;

/// Zbiór bitowy nieaktualnych stron 4 KiB dla jednego obszaru pamięci o rozmiarze `len` bajtów.
pub struct LazyPages {
    bits: Vec<u64>,
    pages: usize,
    len: usize,
}

impl LazyPages {
    /// Obszar `len` bajtów; wszystkie strony świeże. Liczba stron = `len.div_ceil(PAGE)` (ostatnia może być niepełna).
    pub fn new(len: usize) -> LazyPages {
        let pages = len.div_ceil(PAGE);
        LazyPages { bits: vec![0; pages.div_ceil(64)], pages, len }
    }

    /// Oznacza jako nieaktualne wszystkie strony, których dotyka którykolwiek z zakresów `(offset, długość)` w bajtach.
    /// Zakresy mogą się nakładać, mieć długość 0 (ignorowane) i wychodzić poza obszar (przycinane bez paniki).
    pub fn invalidate(&mut self, ranges: &[(u64, u64)]) {
        let limit = self.len as u64;
        for &(off, len) in ranges {
            if len == 0 || off >= limit {
                continue;
            }
            let first = (off as usize) / PAGE;
            let end_byte = off.saturating_add(len).min(limit) as usize;
            let last = (end_byte - 1) / PAGE;
            for p in first..=last {
                self.bits[p / 64] |= 1u64 << (p % 64);
            }
        }
    }

    /// Czy strona `page` jest oznaczona jako nieaktualna (poza zakresem zwraca `false`).
    pub fn is_stale(&self, page: usize) -> bool {
        page < self.pages && (self.bits[page / 64] & (1u64 << (page % 64))) != 0
    }

    /// Liczba nieaktualnych stron w obszarze.
    pub fn stale_count(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Błąd strony pod bajtem `offset`: które bajty trzeba pobrać z serwera. Okno = grupa `around` kolejnych stron wyrównana do
    /// `around` (np. `around = 16` -> strony `0..16`, `16..32`, ...), zawierająca stronę błędu; `around` to potęga dwójki >= 1.
    /// Zwraca scalone, posortowane zakresy bajtów `[od, do)` WYŁĄCZNIE nieaktualnych stron z tego okna (przycięte do `len`).
    /// Jeśli strona błędu jest świeża (albo `offset` poza obszarem): pusty wektor.
    pub fn plan_fetch(&self, offset: usize, around: usize) -> Vec<(usize, usize)> {
        if offset >= self.len || around == 0 {
            return Vec::new();
        }
        let fault_page = offset / PAGE;
        if !self.is_stale(fault_page) {
            return Vec::new();
        }
        let win_start = (fault_page / around) * around;
        let win_end = win_start.saturating_add(around).min(self.pages);
        let mut out: Vec<(usize, usize)> = Vec::new();
        for p in win_start..win_end {
            if self.is_stale(p) {
                let a = p * PAGE;
                let b = ((p + 1) * PAGE).min(self.len);
                match out.last_mut() {
                    Some(last) if last.1 == a => last.1 = b,
                    _ => out.push((a, b)),
                }
            }
        }
        out
    }

    /// Oznacza strony objęte `[from, to)` jako świeże (zakres przycięty do obszaru; niewyrównany początek/koniec obejmuje całe strony,
    /// z wyjątkiem ostatniej niepełnej strony obszaru, która jest świeża, gdy `to >= len`).
    pub fn mark_fresh(&mut self, from: usize, to: usize) {
        let to = to.min(self.len);
        if from >= to {
            return;
        }
        let first = from / PAGE;
        let last = if to >= self.len {
            self.pages - 1
        } else if !self.len.is_multiple_of(PAGE) && to > (self.pages - 1) * PAGE {
            // Ostatnia strona jest niepełna i `to < len`: nie oznaczamy jej jeszcze jako świeżej.
            if self.pages < 2 {
                return;
            }
            self.pages - 2
        } else {
            (to - 1) / PAGE
        };
        if first > last {
            return;
        }
        for p in first..=last {
            self.bits[p / 64] &= !(1u64 << (p % 64));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_area_is_fresh_and_invalidate_handles_overlapping_zero_and_overflowing_ranges() {
        let mut lp = LazyPages::new(PAGE * 10);
        assert_eq!(lp.stale_count(), 0);
        for p in 0..12 {
            assert!(!lp.is_stale(p));
        }

        // Zakresy nakładające się (strony 1..=3), zerowe (ignorowane), wykraczające poza obszar oraz przepełniające u64.
        lp.invalidate(&[
            (PAGE as u64 + 10, PAGE as u64),           // dotyka stron 1 i 2
            (PAGE as u64 * 2 + 500, PAGE as u64 + 20), // dotyka stron 2 i 3 (nakłada się na 2)
            (PAGE as u64 * 5, 0),                      // długość 0 -> ignorowany
            (PAGE as u64 * 8 + 100, PAGE as u64 * 50), // wychodzi poza obszar -> przycięty do stron 8 i 9
            (u64::MAX - 1, 100),                       // przepełnienie u64 poza obszarem -> ignorowany
        ]);

        let stale: Vec<usize> = (0..12).filter(|&p| lp.is_stale(p)).collect();
        assert_eq!(stale, vec![1, 2, 3, 8, 9]);
        assert_eq!(lp.stale_count(), 5);

        // Przepełnienie u64 zaczynające się wewnątrz obszaru przycina się do końca.
        lp.invalidate(&[(PAGE as u64 * 6, u64::MAX)]);
        let stale_after: Vec<usize> = (0..12).filter(|&p| lp.is_stale(p)).collect();
        assert_eq!(stale_after, vec![1, 2, 3, 6, 7, 8, 9]);
        assert_eq!(lp.stale_count(), 7);
    }

    #[test]
    fn plan_fetch_merges_stale_pages_in_aligned_window_and_clips_to_len() {
        let len = PAGE * 20 + 300; // 21 stron (0..=20), ostatnia niepełna (300 B)
        let mut lp = LazyPages::new(len);
        // W oknie 0..16 oznaczamy strony: 2, 3, 4 (ciągłe) oraz 7; w oknie 16..32 strony: 16, 19, 20.
        lp.invalidate(&[
            (PAGE as u64 * 2, PAGE as u64 * 3),
            (PAGE as u64 * 7 + 1, 10),
            (PAGE as u64 * 16, 1),
            (PAGE as u64 * 19, PAGE as u64 * 5),
        ]);

        // Strona świeża oraz offset poza obszarem -> pusty wektor.
        assert!(lp.plan_fetch(PAGE * 5 + 12, 16).is_empty());
        assert!(lp.plan_fetch(len, 16).is_empty());
        assert!(lp.plan_fetch(len + 5000, 16).is_empty());

        // Błąd w stronie 3 -> okno 0..16, scalone (2..5) i osobno (7..8).
        assert_eq!(lp.plan_fetch(PAGE * 3 + 99, 16), vec![(PAGE * 2, PAGE * 5), (PAGE * 7, PAGE * 8)]);

        // Błąd w stronie 19 -> okno 16..32 przycięte do `len` (strona 16 oraz scalone 19..21 z końcem na `len`).
        assert_eq!(lp.plan_fetch(PAGE * 19 + 1, 16), vec![(PAGE * 16, PAGE * 17), (PAGE * 19, len)]);
    }

    #[test]
    fn mark_fresh_and_stale_count_stay_consistent_with_partial_last_page() {
        let len = PAGE * 4 + 123; // 5 stron (0..=4), strona 4 ma 123 bajty
        let mut lp = LazyPages::new(len);
        lp.invalidate(&[(0, len as u64)]);
        assert_eq!(lp.stale_count(), 5);

        // Niewyrównany zakres wewnątrz stron 1..=2 oznacza całe strony 1 i 2 jako świeże.
        lp.mark_fresh(PAGE + 50, PAGE * 2 + 10);
        assert!(!lp.is_stale(1) && !lp.is_stale(2));
        assert_eq!(lp.stale_count(), 3);

        // Częściowy zakres w ostatniej niepełnej stronie (< len) nie oznacza jej jeszcze jako świeżej.
        lp.mark_fresh(PAGE * 4, PAGE * 4 + 50);
        assert!(lp.is_stale(4));
        assert_eq!(lp.stale_count(), 3);

        // Gdy `to >= len`, ostatnia niepełna strona staje się świeża.
        lp.mark_fresh(PAGE * 4, len);
        assert!(!lp.is_stale(4));
        assert_eq!(lp.stale_count(), 2);

        // Oznaczenie reszty z wyjściem poza `len`.
        lp.mark_fresh(0, len + 10000);
        assert_eq!(lp.stale_count(), 0);
    }

    #[test]
    fn window_sizes_single_page_and_empty_area_do_not_panic() {
        // Obszar 0 bajtów.
        let mut empty = LazyPages::new(0);
        assert_eq!(empty.stale_count(), 0);
        empty.invalidate(&[(0, 100), (u64::MAX, u64::MAX)]);
        assert_eq!(empty.stale_count(), 0);
        assert!(!empty.is_stale(0));
        assert!(empty.plan_fetch(0, 1).is_empty());
        assert!(empty.plan_fetch(0, 16).is_empty());
        empty.mark_fresh(0, 100);
        assert_eq!(empty.stale_count(), 0);

        // Obszar jednostronicowy (niepełna strona 500 B), around = 1 i around = 16.
        let mut single = LazyPages::new(500);
        single.invalidate(&[(10, 20)]);
        assert_eq!(single.stale_count(), 1);
        assert_eq!(single.plan_fetch(250, 1), vec![(0, 500)]);
        assert_eq!(single.plan_fetch(250, 16), vec![(0, 500)]);
        single.mark_fresh(0, 500);
        assert_eq!(single.stale_count(), 0);

        // Wielostronicowy z around = 1 (pobiera dokładnie jedną stronę błędu, bez sąsiednich nieaktualnych).
        let mut multi = LazyPages::new(PAGE * 4);
        multi.invalidate(&[(0, PAGE as u64 * 4)]);
        assert_eq!(multi.plan_fetch(PAGE * 2 + 3, 1), vec![(PAGE * 2, PAGE * 3)]);
        assert_eq!(multi.plan_fetch(PAGE * 2 + 3, 16), vec![(0, PAGE * 4)]);
    }
}
