//! Cache wyników `vkGet{Image,Buffer}MemoryRequirements{,2}`: odpowiedź zależy wyłącznie od parametrów utworzenia obrazu lub
//! bufora (i urządzenia), więc identyczne obrazy nie wymagają kolejnego obiegu po sieci. Cache'ujemy tylko przypadki bez `pNext`
//! i bez współdzielenia między rodzinami kolejek, bo tam wynik mógłby zależeć od danych spoza klucza.

use std::collections::HashMap;

/// Wynik zapytania: rozmiar, wyrównanie, maska typów pamięci (`VkMemoryRequirements`) oraz opcjonalnie flagi z
/// `VkMemoryDedicatedRequirements` (`prefers`, `requires`), gdy aplikacja dołączyła tę strukturę do odpowiedzi.
pub type Requirements = (u64, u64, u32, Option<(u32, u32)>);

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
    /// Klucz z `VkImageCreateInfo` (flags @16 ... sharingMode @60 oraz indeksy rodzin kolejek i znane struktury pNext). Wynik zależy
    /// wyłącznie od tych pól, więc włączenie ich do klucza zachowuje poprawność; `None` przy nieznanej strukturze w łańcuchu pNext.
    ///
    /// # Safety
    /// `info` musi wskazywać na poprawną strukturę `VkImageCreateInfo` (88 B) wraz z łańcuchem pNext i tablicą indeksów.
    pub unsafe fn image_key(info: *const u8) -> Option<Vec<u8>> {
        let mut k = std::slice::from_raw_parts(info.add(16), 48).to_vec();
        if rd32(info, 60) != 0 {
            // Współdzielenie między rodzinami kolejek: lista rodzin należy do klucza.
            let n = rd32(info, 64) as usize;
            let list = rd64(info, 72) as *const u8;
            if n > 16 || (n > 0 && list.is_null()) {
                return None;
            }
            k.push(b'Q');
            k.extend_from_slice(&(n as u32).to_le_bytes());
            k.extend_from_slice(std::slice::from_raw_parts(list, n * 4));
        }
        let mut next = rd64(info, 8) as *const u8;
        let mut depth = 0;
        while !next.is_null() {
            depth += 1;
            if depth > 4 {
                return None;
            }
            match rd32(next, 0) {
                // VkImageFormatListCreateInfo: viewFormatCount @16, pViewFormats @24.
                1_000_147_000 => {
                    let n = rd32(next, 16) as usize;
                    let list = rd64(next, 24) as *const u8;
                    if n > 64 || (n > 0 && list.is_null()) {
                        return None;
                    }
                    k.push(b'F');
                    k.extend_from_slice(&(n as u32).to_le_bytes());
                    k.extend_from_slice(std::slice::from_raw_parts(list, n * 4));
                }
                // VkExternalMemoryImageCreateInfo: handleTypes @16.
                1_000_072_001 => {
                    k.push(b'X');
                    k.extend_from_slice(&rd32(next, 16).to_le_bytes());
                }
                // VkImageStencilUsageCreateInfo: stencilUsage @16.
                1_000_246_000 => {
                    k.push(b'S');
                    k.extend_from_slice(&rd32(next, 16).to_le_bytes());
                }
                _ => return None,
            }
            next = rd64(next, 8) as *const u8;
        }
        Some(k)
    }

    /// Klucz z `VkDeviceImageMemoryRequirements` (pCreateInfo @16, planeAspect @24); osobna przestrzeń od kluczy obiektów.
    ///
    /// # Safety
    /// `info` musi wskazywać na poprawną strukturę `VkDeviceImageMemoryRequirements` z poprawnym `pCreateInfo`.
    pub unsafe fn device_image_key(info: *const u8) -> Option<Vec<u8>> {
        if rd64(info, 8) != 0 {
            return None;
        }
        let ci = rd64(info, 16) as *const u8;
        if ci.is_null() {
            return None;
        }
        let mut k = vec![b'D'];
        k.extend_from_slice(&rd32(info, 24).to_le_bytes());
        k.extend_from_slice(&Self::image_key(ci)?);
        Some(k)
    }

    /// Wynik z cache dla klucza bez obiektu (patrz `device_image_key`).
    pub fn lookup_key(&self, device: u64, key: &[u8], want_dedicated: bool) -> Option<Requirements> {
        let r = self.results.get(&(device, key.to_vec())).copied()?;
        (!want_dedicated || r.3.is_some()).then_some(r)
    }

    pub fn store_key(&mut self, device: u64, key: Vec<u8>, r: Requirements) {
        self.results.insert((device, key), r);
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

    /// Wynik z cache; gdy aplikacja prosi też o `VkMemoryDedicatedRequirements` (`want_dedicated`), wpis musi je zawierać.
    pub fn lookup(&self, device: u64, id: u64, want_dedicated: bool) -> Option<Requirements> {
        let k = self.keys.get(&id)?;
        let r = self.results.get(&(device, k.clone())).copied()?;
        (!want_dedicated || r.3.is_some()).then_some(r)
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
        assert_eq!(c.lookup(9, 1, false), None, "pierwsze zapytanie idzie do serwera");
        c.store(9, 1, (4096, 256, 0x3, None));
        assert_eq!(c.lookup(9, 2, false), Some((4096, 256, 0x3, None)), "identyczny obraz trafia w cache");
        assert_eq!(c.lookup(9, 3, false), None, "inny rozmiar: osobny wynik");
        assert_eq!(c.lookup(8, 2, false), None, "inne urządzenie: osobny wynik");
        assert_eq!(c.lookup(9, 2, true), None, "wpis bez flag dedykowanych nie obsłuży zapytania z VkMemoryDedicatedRequirements");
        c.store(9, 2, (4096, 256, 0x3, Some((1, 0))));
        assert_eq!(c.lookup(9, 1, true), Some((4096, 256, 0x3, Some((1, 0)))), "ten sam klucz, wpis rozszerzony o flagi");
        c.object_destroyed(2);
        assert_eq!(c.lookup(9, 2, false), None);
    }

    #[test]
    fn unknown_pnext_and_invalid_queue_lists_are_never_cached_and_known_ones_extend_the_key() {
        // Nieznana struktura w pNext (sType 0x1234) i wskaźnik na nią: bez cache.
        let mut unknown = [0u8; 32];
        unknown[0..4].copy_from_slice(&0x1234u32.to_le_bytes());
        let with_next = image_info(64, 1, 0, unknown.as_ptr() as u64);
        // Współdzielenie z listą rodzin, ale pustym wskaźnikiem: bez cache.
        let mut concurrent = image_info(64, 1, 1, 0);
        concurrent[64..68].copy_from_slice(&2u32.to_le_bytes());
        let bn = buffer_info(1024, 1, 1);
        unsafe {
            assert!(MemReqCache::image_key(with_next.as_ptr()).is_none());
            assert!(MemReqCache::image_key(concurrent.as_ptr()).is_none());
            assert!(MemReqCache::buffer_key(bn.as_ptr()).is_none());
        }
        // Lista formatów w pNext należy do klucza: inna lista = inny klucz, ta sama = ten sam.
        let formats_a = [37u32, 44];
        let formats_b = [37u32, 45];
        let list = |f: &[u32]| {
            let mut b = vec![0u8; 32];
            b[0..4].copy_from_slice(&1_000_147_000u32.to_le_bytes());
            b[16..20].copy_from_slice(&(f.len() as u32).to_le_bytes());
            b[24..32].copy_from_slice(&(f.as_ptr() as u64).to_le_bytes());
            b
        };
        let (la, lb) = (list(&formats_a), list(&formats_b));
        let (ia, ib, ia2) = (image_info(64, 1, 0, la.as_ptr() as u64), image_info(64, 1, 0, lb.as_ptr() as u64), image_info(64, 1, 0, la.as_ptr() as u64));
        unsafe {
            let (ka, kb, ka2) = (MemReqCache::image_key(ia.as_ptr()).unwrap(), MemReqCache::image_key(ib.as_ptr()).unwrap(), MemReqCache::image_key(ia2.as_ptr()).unwrap());
            assert_eq!(ka, ka2);
            assert_ne!(ka, kb);
            assert_ne!(ka, MemReqCache::image_key(image_info(64, 1, 0, 0).as_ptr()).unwrap(), "z listą formatów to inny klucz niż bez niej");
        }
    }

    #[test]
    fn device_image_requirements_use_their_own_key_space() {
        let ci = image_info(128, 3, 0, 0);
        let mut info = [0u8; 32];
        info[16..24].copy_from_slice(&(ci.as_ptr() as u64).to_le_bytes());
        info[24..28].copy_from_slice(&1u32.to_le_bytes());
        let mut c = MemReqCache::default();
        unsafe {
            let k = MemReqCache::device_image_key(info.as_ptr()).unwrap();
            assert_eq!(c.lookup_key(5, &k, false), None);
            c.store_key(5, k.clone(), (8192, 512, 0x7, None));
            assert_eq!(c.lookup_key(5, &k, false), Some((8192, 512, 0x7, None)));
            assert_eq!(c.lookup_key(6, &k, false), None, "inne urządzenie");
            c.object_created(1, MemReqCache::image_key(ci.as_ptr()));
            assert_eq!(c.lookup(5, 1, false), None, "klucze obiektów i klucze urządzenia się nie mieszają");
            info[24..28].copy_from_slice(&2u32.to_le_bytes());
            assert_ne!(MemReqCache::device_image_key(info.as_ptr()).unwrap(), k, "planeAspect należy do klucza");
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
        c.store(1, 1, (1, 1, 1, None));
        assert_eq!(c.lookup(1, 2, false), None);
    }
}
