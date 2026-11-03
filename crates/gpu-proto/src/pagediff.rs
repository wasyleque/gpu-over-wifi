//! Śledzenie zmian pamięci w stronach 4 KiB: obie strony pamiętają skrót każdej strony w stanie, który druga strona już zna,
//! i przesyłają tylko strony o zmienionym skrócie ("tylko różnice"). Nowa pamięć jest wyzerowana, więc zerowe strony nie są wysyłane.

#![allow(clippy::chunks_exact_to_as_chunks, clippy::manual_is_multiple_of)]

pub const PAGE: usize = 4096;

/// Szybki skrót strony: cztery niezależne tory po 8 bajtów (bez łańcucha zależności, więc kilka razy szybszy niż jeden tor),
/// końcówka uzupełniona zerami. Musi być identyczny po obu stronach.
pub fn page_hash(page: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let word = |c: &[u8]| u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
    let mut lanes = [0xcbf2_9ce4_8422_2325u64, 0x1000_0000_01b3, 0x9ae1_6a3b_2f90_404f, 0xc949_d7c7_509e_6557];
    let mut blocks = page.chunks_exact(32);
    for b in &mut blocks {
        for (i, l) in lanes.iter_mut().enumerate() {
            *l = (*l ^ word(&b[i * 8..])).wrapping_mul(K).rotate_left(29);
        }
    }
    let rest = blocks.remainder();
    let mut tail = [0u8; 32];
    tail[..rest.len()].copy_from_slice(rest);
    for (i, l) in lanes.iter_mut().enumerate() {
        *l = (*l ^ word(&tail[i * 8..])).wrapping_mul(K).rotate_left(29);
    }
    lanes.iter().fold(0u64, |h, l| (h ^ l).wrapping_mul(K).rotate_left(31))
}

/// Od tylu stron wzwyż skróty liczy się równolegle (poniżej koszt uruchomienia wątków przewyższa zysk).
const PARALLEL_MIN_PAGES: usize = 512;

/// Skróty pierwszych `n` stron `data` (strona `i` = bajty `i*PAGE..min((i+1)*PAGE, len)`); duże obszary dzielone na wątki.
fn page_hashes(data: &[u8], n: usize, len: usize) -> Vec<u64> {
    let one = |i: usize| page_hash(&data[i * PAGE..((i + 1) * PAGE).min(len)]);
    let threads = std::thread::available_parallelism().map_or(1, |c| c.get()).min(8).min(n / 256);
    if n < PARALLEL_MIN_PAGES || threads < 2 {
        return (0..n).map(one).collect();
    }
    let mut out = vec![0u64; n];
    let per = n.div_ceil(threads);
    std::thread::scope(|sc| {
        for (t, chunk) in out.chunks_mut(per).enumerate() {
            let one = &one;
            sc.spawn(move || {
                for (k, slot) in chunk.iter_mut().enumerate() {
                    *slot = one(t * per + k);
                }
            });
        }
    });
    out
}

/// Skróty stron wspólnie znane obu stronom dla jednego obszaru pamięci o stałym rozmiarze.
#[derive(Debug, Clone)]
pub struct PageTracker {
    known: Vec<u64>,
    len: usize,
}

impl PageTracker {
    /// Obszar `len` bajtów, o którym obie strony zakładają, że jest wyzerowany.
    pub fn new(len: usize) -> PageTracker {
        let zero = page_hash(&[0u8; PAGE]);
        let pages = len.div_ceil(PAGE);
        let mut known = vec![zero; pages];
        if len % PAGE != 0 {
            if let Some(last) = known.last_mut() {
                *last = page_hash(&vec![0u8; len % PAGE]);
            }
        }
        PageTracker { known, len }
    }

    fn page_range(&self, i: usize) -> (usize, usize) {
        (i * PAGE, ((i + 1) * PAGE).min(self.len))
    }

    /// Zakresy bajtów zmienione względem stanu znanego drugiej stronie; zwracane strony uznajemy za wysłane.
    /// `max_bytes` ogranicza rozmiar jednej porcji: zwraca `(zakresy, true)`, gdy zostały jeszcze zmiany.
    pub fn changed(&mut self, data: &[u8], max_bytes: usize) -> (Vec<(usize, usize)>, bool) {
        let mut out: Vec<(usize, usize)> = Vec::new();
        let mut total = 0usize;
        let n = self.known.len().min(data.len().div_ceil(PAGE));
        // Strona niepełna na końcu `data` kończy pętlę (jak dotąd), więc liczymy tylko strony w całości dostępne.
        let ready = (0..n).take_while(|&i| self.page_range(i).1 <= data.len()).count();
        let hashes = page_hashes(data, ready, self.len);
        for (i, &now) in hashes.iter().enumerate() {
            let (a, b) = self.page_range(i);
            if now == self.known[i] {
                continue;
            }
            if total + (b - a) > max_bytes && total > 0 {
                return (out, true);
            }
            self.known[i] = now;
            total += b - a;
            match out.last_mut() {
                Some(last) if last.1 == a => last.1 = b,
                _ => out.push((a, b)),
            }
        }
        (out, false)
    }

    /// Jak `changed`, ale sprawdza tylko strony z `candidates` (zakresy wyrównane do stron, np. z jądrowego śledzenia zapisów):
    /// strony zapisane, lecz o tej samej zawartości, nie są zgłaszane. Bez limitu rozmiaru.
    pub fn changed_within(&mut self, data: &[u8], candidates: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for &(from, to) in candidates {
            for i in (from / PAGE)..to.div_ceil(PAGE).min(self.known.len()) {
                let (a, b) = self.page_range(i);
                if b > data.len() {
                    break;
                }
                let now = page_hash(&data[a..b]);
                if now == self.known[i] {
                    continue;
                }
                self.known[i] = now;
                match out.last_mut() {
                    Some(last) if last.1 == a => last.1 = b,
                    _ => out.push((a, b)),
                }
            }
        }
        out
    }

    /// Zapis `data` pod `offset`: zapamiętuje skróty stron w całości objętych zapisem, liczone z `data` (bez czytania pamięci,
    /// która po stronie serwera bywa wolna, bo write-combined). Strony częściowo objęte zapisem zostają bez zmian.
    pub fn mark_data(&mut self, offset: usize, data: &[u8]) {
        let end = offset + data.len();
        let first = offset.div_ceil(PAGE);
        for i in first..self.known.len() {
            let (a, b) = self.page_range(i);
            if b > end {
                break;
            }
            self.known[i] = page_hash(&data[a - offset..b - offset]);
        }
    }

    /// Strony zakresu `[from, to)` mają już u drugiej strony taką zawartość, jaką teraz ma `data` (po zapisie lub odczycie).
    pub fn mark_synced(&mut self, data: &[u8], from: usize, to: usize) {
        let to = to.min(data.len()).min(self.len);
        if from >= to {
            return;
        }
        for i in (from / PAGE)..=((to - 1) / PAGE) {
            let (a, b) = self.page_range(i);
            if b <= data.len() {
                self.known[i] = page_hash(&data[a..b]);
            }
        }
    }
}

/// Segment odpowiedzi delty: `offset`, `len`, bajty. Format: `u8 more`, potem powtarzane `u64 offset, u32 len, bajty`.
pub fn encode_segments(data: &[u8], ranges: &[(usize, usize)], more: bool) -> Vec<u8> {
    let mut out = vec![more as u8];
    for &(a, b) in ranges {
        out.extend_from_slice(&(a as u64).to_le_bytes());
        out.extend_from_slice(&((b - a) as u32).to_le_bytes());
        out.extend_from_slice(&data[a..b]);
    }
    out
}

/// Odwrotność `encode_segments`: `(więcej, segmenty (offset, bajty))`. `None` przy uciętych lub niespójnych danych.
/// Segmenty odpowiedzi delty: `(offset, bajty)`.
pub type Segments<'a> = Vec<(u64, &'a [u8])>;

pub fn decode_segments(buf: &[u8]) -> Option<(bool, Segments<'_>)> {
    let (&more, mut rest) = buf.split_first()?;
    let mut segs = Vec::new();
    while !rest.is_empty() {
        let off = u64::from_le_bytes(rest.get(..8)?.try_into().ok()?);
        let len = u32::from_le_bytes(rest.get(8..12)?.try_into().ok()?) as usize;
        let body = rest.get(12..12usize.checked_add(len)?)?;
        segs.push((off, body));
        rest = &rest[12 + len..];
    }
    Some((more != 0, segs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_memory_sends_nothing_and_changed_pages_are_coalesced() {
        let mut data = vec![0u8; PAGE * 5 + 100];
        let mut t = PageTracker::new(data.len());
        assert!(t.changed(&data, usize::MAX).0.is_empty(), "zera nie są wysyłane");
        data[PAGE * 2 + 5] = 1;
        data[PAGE * 3] = 2;
        data[PAGE * 5 + 99] = 3; // niepełna ostatnia strona
        let (r, more) = t.changed(&data, usize::MAX);
        assert_eq!((r, more), (vec![(PAGE * 2, PAGE * 4), (PAGE * 5, PAGE * 5 + 100)], false));
        assert!(t.changed(&data, usize::MAX).0.is_empty(), "po wysłaniu brak zmian");
        data[PAGE * 2 + 5] = 9;
        assert_eq!(t.changed(&data, usize::MAX).0, vec![(PAGE * 2, PAGE * 3)]);
    }

    #[test]
    fn the_byte_budget_splits_work_and_nothing_is_lost() {
        let mut data = vec![0u8; PAGE * 6];
        for p in 0..6 {
            data[p * PAGE] = 1;
        }
        let mut t = PageTracker::new(data.len());
        let (a, more_a) = t.changed(&data, PAGE * 2);
        assert_eq!((a, more_a), (vec![(0, PAGE * 2)], true));
        let (b, more_b) = t.changed(&data, PAGE * 2);
        assert_eq!((b, more_b), (vec![(PAGE * 2, PAGE * 4)], true));
        let (c, more_c) = t.changed(&data, PAGE * 2);
        assert_eq!((c, more_c), (vec![(PAGE * 4, PAGE * 6)], false));
    }

    #[test]
    fn synced_ranges_are_not_resent() {
        let mut data = vec![0u8; PAGE * 3];
        let mut t = PageTracker::new(data.len());
        data[PAGE..PAGE * 2].fill(7);
        t.mark_synced(&data, PAGE, PAGE * 2);
        assert!(t.changed(&data, usize::MAX).0.is_empty());
        data[0] = 1;
        assert_eq!(t.changed(&data, usize::MAX).0, vec![(0, PAGE)]);
    }

    #[test]
    fn segments_round_trip_and_reject_truncation() {
        let data: Vec<u8> = (0..255).cycle().take(PAGE * 3).collect();
        let enc = encode_segments(&data, &[(0, 10), (PAGE, PAGE + 20)], true);
        let (more, segs) = decode_segments(&enc).unwrap();
        assert!(more);
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[0].0, segs[0].1), (0, &data[0..10]));
        assert_eq!((segs[1].0, segs[1].1), (PAGE as u64, &data[PAGE..PAGE + 20]));
        assert!(decode_segments(&enc[..enc.len() - 1]).is_none());
        assert!(decode_segments(&[]).is_none());
        assert_eq!(decode_segments(&[0]).unwrap(), (false, vec![]));
    }

    #[test]
    fn mark_data_records_only_fully_covered_pages_from_the_written_bytes() {
        let mut mem = vec![0u8; PAGE * 4];
        let mut t = PageTracker::new(mem.len());
        // Zapis stron 1..3 (pełne) plus ogon strony 3 (niepełny).
        let payload: Vec<u8> = (1..=255u8).cycle().take(PAGE * 2 + 100).collect();
        mem[PAGE..PAGE + payload.len()].copy_from_slice(&payload);
        t.mark_data(PAGE, &payload);
        let (r, _) = t.changed(&mem, usize::MAX);
        assert_eq!(r, vec![(PAGE * 3, PAGE * 4)], "pełne strony 1 i 2 są znane, niepełna strona 3 nie");
    }

    #[test]
    fn candidates_limit_the_scan_and_unchanged_rewrites_are_not_reported() {
        let mut data = vec![0u8; PAGE * 8];
        let mut t = PageTracker::new(data.len());
        data[PAGE * 2] = 1;
        data[PAGE * 6] = 1; // zmiana poza kandydatami: ma nie zostać zgłoszona w tym wywołaniu
        let r = t.changed_within(&data, &[(PAGE * 2, PAGE * 4)]);
        assert_eq!(r, vec![(PAGE * 2, PAGE * 3)]);
        assert!(t.changed_within(&data, &[(PAGE * 2, PAGE * 4)]).is_empty(), "ta sama zawartość, nic nowego");
        assert_eq!(t.changed_within(&data, &[(PAGE * 6, PAGE * 7)]), vec![(PAGE * 6, PAGE * 7)]);
    }

    #[test]
    fn hash_distinguishes_one_byte_and_position() {
        let mut a = vec![0u8; PAGE];
        let h0 = page_hash(&a);
        a[PAGE - 1] = 1;
        let h1 = page_hash(&a);
        a[PAGE - 1] = 0;
        a[0] = 1;
        assert!(h0 != h1 && h1 != page_hash(&a) && h0 != page_hash(&a));
    }
    #[test]
    fn parallel_hashing_of_large_memory_matches_page_by_page_hashing() {
        // 5 MiB + 123 B (niepełna ostatnia strona), zmiany rozsiane po całym obszarze, w tym w ostatniej stronie.
        let len = 5 * 1024 * 1024 + 123;
        let mut data = vec![0u8; len];
        let mut t = PageTracker::new(len);
        assert!(t.changed(&data, usize::MAX).0.is_empty());
        let touched = [0usize, 4095, 4096, 700_000, 2_500_000, 5_000_000, len - 1];
        for &o in &touched {
            data[o] = 0xAB;
        }
        let (ranges, more) = t.changed(&data, usize::MAX);
        assert!(!more);
        let mut pages: Vec<usize> = touched.iter().map(|o| o / PAGE).collect();
        pages.dedup();
        let got: Vec<usize> = ranges.iter().flat_map(|&(a, b)| (a / PAGE)..b.div_ceil(PAGE)).collect();
        assert_eq!(got, pages);
        // Po zatwierdzeniu nic się już nie zmienia.
        assert!(t.changed(&data, usize::MAX).0.is_empty());
    }

}
