//! Które zakresy odwzorowanej pamięci GPU może zapisać: tylko tam trzeba porównywać zawartość po oczekiwaniu na GPU.
//! Zapisywać można przez bufory z odpowiednimi flagami użycia (i pamięć obrazów liniowych), więc zakresy zapisywalne to
//! `[przesunięcie, przesunięcie + rozmiar)` buforów z flagami zapisu przypisanych do pamięci. Wszystko, czego nie umiemy
//! policzyć (obraz, nieznany bufor, rozszerzenie w pNext), oznacza "cała pamięć": zachowanie zachowawcze.

use std::collections::HashMap;

/// Bity użycia bufora, przy których GPU może go tylko czytać (TRANSFER_SRC, UNIFORM_TEXEL, UNIFORM, INDEX, VERTEX, INDIRECT).
/// Każdy inny bit (też nieznany, w tym SHADER_DEVICE_ADDRESS, który pozwala pisać przez wskaźnik) oznacza możliwy zapis.
const READ_ONLY_BUFFER_USAGE: u32 = 0x1 | 0x4 | 0x10 | 0x40 | 0x80 | 0x100;

/// VK_BUFFER_USAGE_TRANSFER_DST_BIT.
const TRANSFER_DST: u32 = 0x2;
/// VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT, SAMPLER_DESCRIPTOR_BUFFER_BIT_EXT, RESOURCE_DESCRIPTOR_BUFFER_BIT_EXT.
const DEVICE_ADDRESS: u32 = 0x20000;
const DESCRIPTOR_BUFFERS: u32 = 0x200000 | 0x400000;

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
    /// Przypisano bufor, który GPU może zapisać także inaczej niż transferem (storage, adres urządzenia, nieznane użycie).
    other: bool,
    /// Przypisano bufor, który nie jest czystym buforem deskryptorów (użycie poza bitami do odczytu, deskryptorów i adresu urządzenia, albo bez bitu deskryptorów).
    non_desc: bool,
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
        if !matches!(self.bufs.get(&buffer), Some(&(_, usage)) if usage != 0 && usage & !(READ_ONLY_BUFFER_USAGE | TRANSFER_DST) == 0) {
            e.other = true;
        }
        if !matches!(self.bufs.get(&buffer), Some(&(_, usage)) if usage & DESCRIPTOR_BUFFERS != 0 && usage & !(READ_ONLY_BUFFER_USAGE | DESCRIPTOR_BUFFERS | DEVICE_ADDRESS) == 0) {
            e.non_desc = true;
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

    /// Do pamięci przypisano wyłącznie bufory, które GPU może zapisać tylko poleceniami transferu (użycie: bity tylko do odczytu i TRANSFER_DST).
    pub fn transfer_only(&self, memory: u64) -> bool {
        self.mems.get(&memory).is_some_and(|e| !e.whole && !e.other)
    }

    /// Do pamięci przypisano wyłącznie bufory deskryptorów (VK_EXT_descriptor_buffer): zapisuje je procesor, a GPU tylko czyta. Jedyna droga zapisu przez
    /// GPU to wskaźnik z adresu urządzenia, której nie wykluczamy statycznie: klient pomija odświeżanie, ale audytuje (patrz `fwd::xfer_audit_pick`).
    pub fn descriptor_only(&self, memory: u64) -> bool {
        self.mems.get(&memory).is_some_and(|e| !e.whole && !e.non_desc)
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
    fn transfer_only_memories_are_recognised_by_usage() {
        let mut g = GpuWrites::default();
        g.buffer_created(1, 4096, 0x1 | 0x2 | 0x4); // jak bufory vkd3d: transfer + teksel uniform
        g.buffer_created(2, 4096, 0x20 | 0x2); // storage
        g.buffer_created(3, 4096, 0x2 | 0x20000); // adres urządzenia
        g.bind_buffer(10, 1, 0);
        assert!(g.transfer_only(10));
        g.bind_buffer(11, 1, 0);
        g.bind_buffer(11, 2, 4096);
        assert!(!g.transfer_only(11), "storage");
        g.bind_buffer(12, 3, 0);
        assert!(!g.transfer_only(12), "adres urządzenia");
        g.bind_image(13);
        assert!(!g.transfer_only(13) && !g.transfer_only(99));
        g.bind_buffer(14, 77, 0); // nieznany bufor
        assert!(!g.transfer_only(14));
    }

    #[test]
    fn descriptor_buffer_memories_are_recognised() {
        let mut g = GpuWrites::default();
        g.buffer_created(1, 8 << 20, 0x420000); // resource descriptor + adres
        g.buffer_created(2, 8192, 0x220000); // sampler descriptor + adres
        g.buffer_created(3, 4096, 0x20000 | 0x20); // storage + adres: zwykły bufor
        g.buffer_created(4, 4096, 0x20000); // sam adres
        g.bind_buffer(10, 1, 0);
        g.bind_buffer(10, 2, 8 << 20);
        assert!(g.descriptor_only(10));
        g.bind_buffer(11, 1, 0);
        g.bind_buffer(11, 3, 8 << 20);
        assert!(!g.descriptor_only(11));
        g.bind_buffer(12, 4, 0);
        assert!(!g.descriptor_only(12) && !g.descriptor_only(99));
        g.bind_image(13);
        assert!(!g.descriptor_only(13));
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
