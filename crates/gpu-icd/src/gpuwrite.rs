//! Które zakresy odwzorowanej pamięci GPU może zapisać: tylko tam trzeba porównywać zawartość po oczekiwaniu na GPU.
//! Zapisywać można przez bufory z odpowiednimi flagami użycia (i pamięć obrazów liniowych), więc zakresy zapisywalne to
//! `[przesunięcie, przesunięcie + rozmiar)` buforów z flagami zapisu przypisanych do pamięci. Wszystko, czego nie umiemy
//! policzyć (obraz, nieznany bufor, rozszerzenie w pNext), oznacza "cała pamięć": zachowanie zachowawcze.

use std::collections::HashMap;

/// Bity użycia bufora, przy których GPU może go tylko czytać (TRANSFER_SRC, UNIFORM_TEXEL, UNIFORM, INDEX, VERTEX, INDIRECT).
/// Każdy inny bit (też nieznany, w tym SHADER_DEVICE_ADDRESS, który pozwala pisać przez wskaźnik) oznacza możliwy zapis.
const READ_ONLY_BUFFER_USAGE: u32 = 0x1 | 0x4 | 0x10 | 0x40 | 0x80 | 0x100;

pub fn usage_is_read_only(usage: u32) -> bool {
    usage != 0 && usage & !READ_ONLY_BUFFER_USAGE == 0
}

/// Co odświeżać w pamięci po oczekiwaniu na GPU.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// GPU nie może tu pisać: nic do odświeżenia.
    Skip,
    /// Nie wiadomo, gdzie GPU pisze: cała pamięć.
    Whole,
    /// Tylko te zakresy `(offset, długość)`, posortowane i scalone.
    Ranges(Vec<(u64, u64)>),
}

#[derive(Default)]
struct MemUse {
    /// Przypisano coś, czego zakresu nie umiemy określić (obraz, nieznany bufor).
    whole: bool,
    ranges: Vec<(u64, u64)>,
    /// Diagnostyka (do logu): pierwsze bufory przypisane do pamięci jako (przesunięcie, rozmiar, flagi użycia).
    sample: Vec<(u64, u64, u32)>,
}

/// Bufory (rozmiar, użycie) i to, co przypisano do poszczególnych pamięci.
#[derive(Default)]
pub struct GpuWrites {
    bufs: HashMap<u64, (u64, u32)>,
    mems: HashMap<u64, MemUse>,
}

impl GpuWrites {
    pub fn buffer_created(&mut self, id: u64, size: u64, usage: u32) {
        self.bufs.insert(id, (size, usage));
    }

    pub fn buffer_destroyed(&mut self, id: u64) {
        self.bufs.remove(&id);
    }

    pub fn memory_freed(&mut self, memory: u64) {
        self.mems.remove(&memory);
    }

    pub fn bind_buffer(&mut self, memory: u64, buffer: u64, offset: u64) {
        let e = self.mems.entry(memory).or_default();
        if let Some(&(size, usage)) = self.bufs.get(&buffer) {
            if e.sample.len() < 6 {
                e.sample.push((offset, size, usage));
            }
        }
        match self.bufs.get(&buffer) {
            Some(&(_, usage)) if usage_is_read_only(usage) => {}
            Some(&(size, _)) if size > 0 => e.ranges.push((offset, size)),
            _ => e.whole = true,
        }
    }

    pub fn bind_image(&mut self, memory: u64) {
        self.mems.entry(memory).or_default().whole = true;
    }

    /// Krótki opis do logu: z czego wynika plan odświeżania pamięci (bufory i ich flagi użycia).
    pub fn describe(&self, memory: u64) -> String {
        match self.mems.get(&memory) {
            None => "brak powiązań".into(),
            Some(e) => format!("obrazy:{} bufory(offset,rozmiar,użycie): {:x?}", e.whole, e.sample),
        }
    }

    pub fn plan(&self, memory: u64) -> Plan {
        let Some(e) = self.mems.get(&memory) else { return Plan::Whole };
        if e.whole {
            return Plan::Whole;
        }
        if e.ranges.is_empty() {
            return Plan::Skip;
        }
        let mut r = e.ranges.clone();
        r.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (off, len) in r {
            match merged.last_mut() {
                Some(last) if off <= last.0.saturating_add(last.1) => last.1 = last.1.max(off.saturating_add(len) - last.0),
                _ => merged.push((off, len)),
            }
        }
        // Za dużo zakresów to koszt większy niż zysk (i limit protokołu): wtedy cała pamięć.
        if merged.len() > gpu_proto::msg::MAX_DELTA_RANGES {
            return Plan::Whole;
        }
        Plan::Ranges(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_follows_what_is_bound_to_the_memory() {
        let mut g = GpuWrites::default();
        assert_eq!(g.plan(1), Plan::Whole, "pamięć bez znanych powiązań");
        g.buffer_created(10, 4096, 0x80); // VERTEX
        g.buffer_created(11, 8192, 0x20 | 0x80); // STORAGE | VERTEX
        g.buffer_created(12, 4096, 0x2); // TRANSFER_DST
        g.bind_buffer(1, 10, 0);
        assert_eq!(g.plan(1), Plan::Skip, "tylko bufor do odczytu");
        g.bind_buffer(1, 11, 65536);
        g.bind_buffer(1, 12, 73728);
        g.bind_buffer(1, 12, 73728 + 100); // nakłada się: scalane
        // (65536, 8192) + (73728, 4096) + (73828, 4096) -> jeden zakres od 65536 do 73828 + 4096.
        assert_eq!(g.plan(1), Plan::Ranges(vec![(65536, 73828 + 4096 - 65536)]));
    }

    #[test]
    fn images_unknown_buffers_and_many_ranges_mean_whole_memory() {
        let mut g = GpuWrites::default();
        g.bind_image(2);
        assert_eq!(g.plan(2), Plan::Whole);
        g.bind_buffer(3, 99, 0); // bufor nieznany
        assert_eq!(g.plan(3), Plan::Whole);
        g.buffer_created(5, 16, 0x2);
        for i in 0..(gpu_proto::msg::MAX_DELTA_RANGES as u64 + 5) {
            g.bind_buffer(4, 5, i * 64);
        }
        assert_eq!(g.plan(4), Plan::Whole);
        g.memory_freed(2);
        assert_eq!(g.plan(2), Plan::Whole, "po zwolnieniu brak wpisu = cała");
    }

    #[test]
    fn read_only_usage_rules() {
        assert!(usage_is_read_only(0x1) && usage_is_read_only(0x10 | 0x80 | 0x40));
        assert!(!usage_is_read_only(0) && !usage_is_read_only(0x2) && !usage_is_read_only(0x10 | 0x20));
        assert!(!usage_is_read_only(0x20000) && !usage_is_read_only(0x1 | 0x8000_0000) && !usage_is_read_only(u32::MAX));
    }
}
