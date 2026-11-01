//! Budowa łańcucha `pNext` dla zapytań `vkGetPhysicalDevice*2` na podstawie listy `sType` od klienta.
//! Bufor to `Vec<u64>` (wyrównanie 8 B), a `pNext` zawiera PRAWDZIWE adresy wewnątrz bufora.
//! Układ każdej struktury Vulkana na x86_64: `sType` (u32) w offsecie 0, `pNext` (wskaźnik) w offsecie 8.

use std::ffi::c_void;

use gpu_proto::stype::lookup;

pub const MAX_CHAIN: usize = 64;
pub const PNEXT_OFFSET: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainError {
    TooLong,
    Duplicate(u32),
    Unknown(u32),
    /// Struktura ma członka-wskaźnika poza `pNext`: nie wolno jej wystawiać klientowi.
    UnsupportedOutputPointers(u32),
}

pub struct ChainBuf {
    // Pola wybiera implementacja: bufor `Vec<u64>`, offsety struktur w bajtach i ich `sType`.
    _private: (),
}

/// Buduje łańcuch w kolejności z `chain`. Każda struktura leży w offsecie wyrównanym do
/// `max(align, 8)`, ma wyzerowane wszystkie pola poza `sType` i `pNext`; `pNext` ostatniej to null.
/// Pusta lista daje pusty łańcuch (`head_ptr()` == null). Walidacja przed jakąkolwiek alokacją
/// zależną od danych klienta: długość > MAX_CHAIN, duplikat sType, nieznany sType, wskaźniki wyjściowe.
pub fn build_chain(chain: &[u32]) -> Result<ChainBuf, ChainError> {
    todo!()
}

impl ChainBuf {
    /// Adres pierwszej struktury (do wstawienia w `pNext` głowy zapytania) albo null dla pustego łańcucha.
    pub fn head_ptr(&mut self) -> *mut c_void {
        todo!()
    }

    /// Liczba struktur w łańcuchu.
    pub fn len(&self) -> usize {
        todo!()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Cały bufor jako bajty (struktury jedna po drugiej, z wypełnieniem).
    pub fn as_bytes(&self) -> &[u8] {
        todo!()
    }

    /// Bajty do odpowiedzi dla klienta: kopia bufora, w której `pNext` KAŻDEJ struktury jest wyzerowany
    /// (klient nie może dostać adresów serwera).
    pub fn into_response_bytes(self) -> Vec<u8> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpu_proto::stype::STYPE_TABLE;

    fn st(name: &str) -> u32 {
        STYPE_TABLE.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("brak {name}")).stype
    }

    fn size_of_stype(s: u32) -> usize {
        lookup(s).unwrap().size as usize
    }

    fn read_u32(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }

    fn read_u64(b: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }

    #[test]
    fn empty_chain_is_valid_and_has_null_head() {
        let mut c = build_chain(&[]).unwrap();
        assert!(c.head_ptr().is_null());
        assert_eq!(c.len(), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn single_structure_has_stype_zeroed_body_and_null_pnext() {
        let s = st("VkPhysicalDeviceVulkan12Features");
        let mut c = build_chain(&[s]).unwrap();
        assert_eq!(c.len(), 1);
        assert!(!c.head_ptr().is_null());
        let b = c.as_bytes();
        assert!(b.len() >= size_of_stype(s));
        assert_eq!(read_u32(b, 0), s);
        assert_eq!(read_u64(b, PNEXT_OFFSET), 0, "ostatni pNext to null");
        assert!(b[16..size_of_stype(s)].iter().all(|&x| x == 0), "reszta wyzerowana");
    }

    #[test]
    fn pnext_values_are_real_addresses_inside_the_buffer() {
        let ids = [st("VkPhysicalDeviceVulkan11Features"), st("VkPhysicalDeviceVulkan12Features"), st("VkPhysicalDeviceVulkan13Features")];
        let mut c = build_chain(&ids).unwrap();
        let head = c.head_ptr() as usize;
        let base = c.as_bytes().as_ptr() as usize;
        assert_eq!(head, base, "głowa to początek bufora");
        assert_eq!(head % 8, 0, "wyrównanie do 8");
        let total = c.as_bytes().len();
        // Idziemy po łańcuchu przez pNext i odczytujemy sType kolejnych struktur.
        let mut cur = head;
        let mut seen = Vec::new();
        for _ in 0..ids.len() {
            let off = cur - base;
            assert!(off < total, "adres poza buforem");
            assert_eq!(cur % 8, 0, "każda struktura wyrównana do 8");
            seen.push(read_u32(c.as_bytes(), off));
            cur = read_u64(c.as_bytes(), off + PNEXT_OFFSET) as usize;
            if cur == 0 {
                break;
            }
        }
        assert_eq!(seen, ids.to_vec());
        assert_eq!(cur, 0, "łańcuch kończy się nullem");
    }

    #[test]
    fn structures_do_not_overlap() {
        let ids = [st("VkPhysicalDeviceVulkan11Properties"), st("VkPhysicalDeviceVulkan12Properties"), st("VkPhysicalDeviceVulkan13Properties")];
        let mut c = build_chain(&ids).unwrap();
        let base = c.as_bytes().as_ptr() as usize;
        let mut cur = c.head_ptr() as usize;
        let mut spans = Vec::new();
        while cur != 0 {
            let off = cur - base;
            let s = read_u32(c.as_bytes(), off);
            spans.push((off, off + size_of_stype(s)));
            cur = read_u64(c.as_bytes(), off + PNEXT_OFFSET) as usize;
        }
        assert_eq!(spans.len(), 3);
        for w in spans.windows(2) {
            assert!(w[0].1 <= w[1].0, "nakładanie: {spans:?}");
        }
        assert!(spans.last().unwrap().1 <= c.as_bytes().len());
    }

    #[test]
    fn response_bytes_have_all_pnext_zeroed_but_keep_stypes() {
        let ids = [st("VkPhysicalDeviceVulkan11Features"), st("VkPhysicalDeviceVulkan12Features")];
        let mut c = build_chain(&ids).unwrap();
        let base = c.as_bytes().as_ptr() as usize;
        let first = c.head_ptr() as usize - base;
        let second = read_u64(c.as_bytes(), first + PNEXT_OFFSET) as usize - base;
        assert_ne!(read_u64(c.as_bytes(), first + PNEXT_OFFSET), 0);
        let out = c.into_response_bytes();
        assert_eq!(read_u64(&out, first + PNEXT_OFFSET), 0);
        assert_eq!(read_u64(&out, second + PNEXT_OFFSET), 0);
        assert_eq!(read_u32(&out, first), ids[0]);
        assert_eq!(read_u32(&out, second), ids[1]);
    }

    #[test]
    fn duplicates_unknown_and_too_long_are_rejected() {
        let a = st("VkPhysicalDeviceVulkan12Features");
        assert_eq!(build_chain(&[a, a]).err(), Some(ChainError::Duplicate(a)));
        assert_eq!(build_chain(&[999_999_999]).err(), Some(ChainError::Unknown(999_999_999)));
        assert_eq!(build_chain(&[u32::MAX]).err(), Some(ChainError::Unknown(u32::MAX)));
        let many: Vec<u32> = (0..MAX_CHAIN as u32 + 1).map(|i| 2_000_000_000 + i).collect();
        assert_eq!(build_chain(&many).err(), Some(ChainError::TooLong), "długość sprawdzana przed szukaniem sType");
    }

    #[test]
    fn structures_with_output_pointers_are_rejected() {
        let bad = st("VkInstanceCreateInfo");
        assert!(lookup(bad).unwrap().has_output_pointers);
        assert_eq!(build_chain(&[bad]).err(), Some(ChainError::UnsupportedOutputPointers(bad)));
        let ok = st("VkPhysicalDeviceVulkan12Features");
        assert_eq!(build_chain(&[ok, bad]).err(), Some(ChainError::UnsupportedOutputPointers(bad)));
    }

    #[test]
    fn exactly_max_chain_valid_structures_is_accepted() {
        // Bierzemy MAX_CHAIN różnych struktur bez wskaźników z tablicy.
        let ids: Vec<u32> = STYPE_TABLE.iter().filter(|e| !e.has_output_pointers && e.size >= 16).take(MAX_CHAIN).map(|e| e.stype).collect();
        assert_eq!(ids.len(), MAX_CHAIN);
        let mut c = build_chain(&ids).unwrap();
        assert_eq!(c.len(), MAX_CHAIN);
        let base = c.as_bytes().as_ptr() as usize;
        let mut cur = c.head_ptr() as usize;
        let mut n = 0;
        while cur != 0 {
            n += 1;
            cur = read_u64(c.as_bytes(), cur - base + PNEXT_OFFSET) as usize;
            assert!(n <= MAX_CHAIN);
        }
        assert_eq!(n, MAX_CHAIN);
    }

    #[test]
    fn random_chains_never_panic() {
        let ids: Vec<u32> = STYPE_TABLE.iter().map(|e| e.stype).collect();
        let mut x: u64 = 0x1234_5678_9ABC_DEF1;
        let mut next = || {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        for _ in 0..2000 {
            let len = (next() % 70) as usize;
            let chain: Vec<u32> = (0..len)
                .map(|_| if next() % 4 == 0 { next() as u32 } else { ids[(next() as usize) % ids.len()] })
                .collect();
            if let Ok(mut c) = build_chain(&chain) {
                let _ = c.head_ptr();
                let _ = c.into_response_bytes();
            }
        }
    }
}
