//! Księgowość gorących i zimnych bloków areny (czysta logika, bez wywołań systemowych; rejestracja w userfaultfd jest gdzie indziej).
//! Blok jest gorący, gdy był zapisany w ostatnich `cool_after` skanach; zimne bloki chroni pułapka synchroniczna, a pierwszy zapis w zimnym bloku
//! (`promote`) robi z niego gorący i oznacza go jako cały brudny (nie wiemy, które strony zdążyły się zmienić w oknie przenoszenia).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Cold,
    /// Numer ostatniego skanu, w którym blok był zapisany lub awansowany.
    Hot(u64),
}

pub struct HotSet {
    blocks: Vec<State>,
    cool_after: u64,
    /// Bloki, które po awansie mają być wysłane w całości przy następnym skanie.
    whole: Vec<bool>,
}

impl HotSet {
    /// Wszystkie bloki startują jako gorące (bezpiecznie: pierwsze skany obejmują całość, chłodzenie zaczyna się po `cool_after` skanach bez zapisu).
    pub fn new(blocks: usize, cool_after: u64) -> HotSet {
        HotSet { blocks: vec![State::Hot(0); blocks], cool_after, whole: vec![false; blocks] }
    }

    pub fn is_hot(&self, b: usize) -> bool {
        matches!(self.blocks.get(b), Some(State::Hot(_)))
    }

    /// Awans zimnego bloku po zapisie (błąd synchroniczny); zwraca `true`, jeśli blok był zimny. Cały blok liczy się jako brudny.
    pub fn promote(&mut self, b: usize, scan: u64) -> bool {
        match self.blocks.get_mut(b) {
            Some(s @ State::Cold) => {
                *s = State::Hot(scan);
                self.whole[b] = true;
                true
            }
            _ => false,
        }
    }

    /// Odnotowuje bloki zapisane w skanie `scan`, zwraca bloki do ochłodzenia (gorące bez zapisu w ostatnich `cool_after` skanach). Chłodzenie
    /// wykonuje wołający (UNREGISTER/REGISTER + weryfikacja) i potwierdza je przez `cooled`.
    pub fn on_scan(&mut self, scan: u64, written: &[usize]) -> Vec<usize> {
        for &b in written {
            if let Some(s) = self.blocks.get_mut(b) {
                *s = State::Hot(scan);
            }
        }
        self.blocks
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                State::Hot(last) if scan.saturating_sub(*last) >= self.cool_after => Some(i),
                _ => None,
            })
            .collect()
    }

    pub fn cooled(&mut self, b: usize) {
        if let Some(s) = self.blocks.get_mut(b) {
            *s = State::Cold;
        }
    }

    /// Bloki, które po awansie trzeba wysłać w całości; zeruje znacznik.
    pub fn take_whole(&mut self) -> Vec<usize> {
        let v: Vec<usize> = self.whole.iter().enumerate().filter(|(_, w)| **w).map(|(i, _)| i).collect();
        for &b in &v {
            self.whole[b] = false;
        }
        v
    }

    /// Przedziały `[od, do)` bloków gorących (sąsiednie scalone): tyle skanuje `PAGEMAP_SCAN`.
    pub fn hot_extents(&self) -> Vec<(usize, usize)> {
        let mut v: Vec<(usize, usize)> = Vec::new();
        for (i, s) in self.blocks.iter().enumerate() {
            if matches!(s, State::Hot(_)) {
                match v.last_mut() {
                    Some(l) if l.1 == i => l.1 = i + 1,
                    _ => v.push((i, i + 1)),
                }
            }
        }
        v
    }

    pub fn hot_count(&self) -> usize {
        self.blocks.iter().filter(|s| matches!(s, State::Hot(_))).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_starts_hot_and_is_scanned_whole() {
        let h = HotSet::new(8, 10);
        assert_eq!(h.hot_extents(), vec![(0, 8)]);
        assert_eq!(h.hot_count(), 8);
    }

    #[test]
    fn blocks_without_writes_cool_only_after_the_window() {
        let mut h = HotSet::new(4, 3);
        assert!(h.on_scan(1, &[0]).is_empty());
        assert!(h.on_scan(2, &[0]).is_empty());
        assert_eq!(h.on_scan(3, &[0]), vec![1, 2, 3]); // 1..3 nie były zapisane od skanu 0
        for b in [1, 2, 3] {
            h.cooled(b);
        }
        assert_eq!(h.hot_extents(), vec![(0, 1)]);
        assert!(h.on_scan(4, &[0]).is_empty());
    }

    #[test]
    fn a_written_block_never_cools() {
        let mut h = HotSet::new(2, 2);
        for s in 1..50 {
            assert!(!h.on_scan(s, &[0]).contains(&0));
        }
    }

    #[test]
    fn promoting_a_cold_block_makes_it_hot_and_whole_once() {
        let mut h = HotSet::new(3, 1);
        let c = h.on_scan(5, &[]);
        for b in c {
            h.cooled(b);
        }
        assert_eq!(h.hot_count(), 0);
        assert!(h.promote(1, 6));
        assert!(!h.promote(1, 6), "drugi błąd w tym samym bloku nic nie zmienia");
        assert_eq!(h.hot_extents(), vec![(1, 2)]);
        assert_eq!(h.take_whole(), vec![1]);
        assert!(h.take_whole().is_empty());
    }

    #[test]
    fn extents_merge_neighbours_and_ignore_out_of_range() {
        let mut h = HotSet::new(6, 1);
        for b in h.on_scan(9, &[1, 2, 4]) {
            h.cooled(b);
        }
        assert_eq!(h.hot_extents(), vec![(1, 3), (4, 5)]);
        assert!(!h.promote(99, 1));
        h.on_scan(10, &[99]);
    }
}
