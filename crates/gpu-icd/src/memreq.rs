//! Cache wyników `vkGet{Image,Buffer}MemoryRequirements{,2}`: odpowiedź zależy wyłącznie od parametrów utworzenia obrazu lub
//! bufora (i urządzenia), więc identyczne obrazy nie wymagają kolejnego obiegu po sieci. Cache'ujemy tylko przypadki bez `pNext`
//! i bez współdzielenia między rodzinami kolejek, bo tam wynik mógłby zależeć od danych spoza klucza.

use std::collections::HashMap;

/// Wynik zapytania: rozmiar, wyrównanie, maska typów pamięci (`VkMemoryRequirements`).
pub type Requirements = (u64, u64, u32);

#[derive(Default)]
pub struct MemReqCache {
    /// Identyfikator obrazu/bufora -> klucz utworzenia.
    keys: HashMap<u64, Vec<u8>>,
    /// (urządzenie, klucz) -> wynik.
    results: HashMap<(u64, Vec<u8>), Requirements>,
}

fn rd32(p: *const u8, o: usize) -> u32 {
    // SAFETY: wołający gwarantuje, że `p` wskazuje na strukturę co najmniej tak długą jak `o + 4`.
    unsafe { std::ptr::read_unaligned(p.add(o) as *const u32) }
}

fn rd64(p: *const u8, o: usize) -> u64 {
    // SAFETY: jak wyżej, dla `o + 8`.
    unsafe { std::ptr::read_unaligned(p.add(o) as *const u64) }
}

impl MemReqCache {
    /// Klucz z `VkImageCreateInfo` (flags @16 ... sharingMode @60); `None`, gdy nie wolno cache'ować (pNext, współdzielenie).
    ///
    /// # Safety
    /// `info` musi wskazywać na poprawną strukturę `VkImageCreateInfo` (88 B).
    pub unsafe fn image_key(info: *const u8) -> Option<Vec<u8>> {
        if rd64(info, 8) != 0 || rd32(info, 60) != 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(info.add(16), 48).to_vec())
    }

    /// Klucz z `VkBufferCreateInfo` (flags @16, size @24, usage @32); `None`, gdy pNext albo współdzielenie.
    ///
    /// # Safety
    /// `info` musi wskazywać na poprawną strukturę `VkBufferCreateInfo` (56 B).
    pub unsafe fn buffer_key(info: *const u8) -> Option<Vec<u8>> {
        if rd64(info, 8) != 0 || rd32(info, 36) != 0 {
            return None;
        }
        let mut k = vec![b'B'];
        k.extend_from_slice(std::slice::from_raw_parts(info.add(16), 24));
        Some(k)
    }

    pub fn object_created(&mut self, id: u64, key: Option<Vec<u8>>) {
        match key {
            Some(k) => {
                self.keys.insert(id, k);
            }
            None => {
                self.keys.remove(&id);
            }
        }
    }

    pub fn object_destroyed(&mut self, id: u64) {
        self.keys.remove(&id);
    }

    pub fn lookup(&self, device: u64, id: u64) -> Option<Requirements> {
        let k = self.keys.get(&id)?;
        self.results.get(&(device, k.clone())).copied()
    }

    pub fn store(&mut self, device: u64, id: u64, r: Requirements) {
        if let Some(k) = self.keys.get(&id) {
            self.results.insert((device, k.clone()), r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_info(w: u32, usage: u32, sharing: u32, pnext: u64) -> Vec<u8> {
        let mut b = vec![0u8; 88];
        b[8..16].copy_from_slice(&pnext.to_le_bytes());
        b[28..32].copy_from_slice(&w.to_le_bytes());
        b[56..60].copy_from_slice(&usage.to_le_bytes());
        b[60..64].copy_from_slice(&sharing.to_le_bytes());
        b
    }

    fn buffer_info(size: u64, usage: u32, sharing: u32) -> Vec<u8> {
        let mut b = vec![0u8; 56];
        b[24..32].copy_from_slice(&size.to_le_bytes());
        b[32..36].copy_from_slice(&usage.to_le_bytes());
        b[36..40].copy_from_slice(&sharing.to_le_bytes());
        b
    }

    #[test]
    fn identical_images_share_a_result_and_different_ones_do_not() {
        let mut c = MemReqCache::default();
        let (a, b, d) = (image_info(256, 7, 0, 0), image_info(256, 7, 0, 0), image_info(512, 7, 0, 0));
        unsafe {
            c.object_created(1, MemReqCache::image_key(a.as_ptr()));
            c.object_created(2, MemReqCache::image_key(b.as_ptr()));
            c.object_created(3, MemReqCache::image_key(d.as_ptr()));
        }
        assert_eq!(c.lookup(9, 1), None, "pierwsze zapytanie idzie do serwera");
        c.store(9, 1, (4096, 256, 0x3));
        assert_eq!(c.lookup(9, 2), Some((4096, 256, 0x3)), "identyczny obraz trafia w cache");
        assert_eq!(c.lookup(9, 3), None, "inny rozmiar: osobny wynik");
        assert_eq!(c.lookup(8, 2), None, "inne urządzenie: osobny wynik");
        c.object_destroyed(2);
        assert_eq!(c.lookup(9, 2), None);
    }

    #[test]
    fn pnext_and_concurrent_sharing_are_never_cached() {
        let with_next = image_info(64, 1, 0, 0xDEAD);
        let concurrent = image_info(64, 1, 1, 0);
        let bn = buffer_info(1024, 1, 1);
        unsafe {
            assert!(MemReqCache::image_key(with_next.as_ptr()).is_none());
            assert!(MemReqCache::image_key(concurrent.as_ptr()).is_none());
            assert!(MemReqCache::buffer_key(bn.as_ptr()).is_none());
        }
    }

    #[test]
    fn buffers_and_images_never_collide() {
        let mut c = MemReqCache::default();
        let i = image_info(0, 0, 0, 0);
        let b = buffer_info(0, 0, 0);
        unsafe {
            c.object_created(1, MemReqCache::image_key(i.as_ptr()));
            c.object_created(2, MemReqCache::buffer_key(b.as_ptr()));
        }
        c.store(1, 1, (1, 1, 1));
        assert_eq!(c.lookup(1, 2), None);
    }
}
