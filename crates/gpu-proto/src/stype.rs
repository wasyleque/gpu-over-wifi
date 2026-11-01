//! Wyszukiwanie struktur Vulkana po numerze `sType` (tablica generowana z `vk.xml`).

pub use crate::stype_table::{StypeEntry, STYPE_TABLE};

/// Zwraca wpis dla danego `sType` albo `None`, gdy struktura jest nieznana.
pub fn lookup(stype: u32) -> Option<&'static StypeEntry> {
    STYPE_TABLE.binary_search_by_key(&stype, |e| e.stype).ok().map(|i| &STYPE_TABLE[i])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn table_is_strictly_sorted_so_binary_search_is_valid() {
        assert!(STYPE_TABLE.windows(2).all(|w| w[0].stype < w[1].stype));
        assert!(STYPE_TABLE.len() > 900);
    }

    #[test]
    fn known_structures_are_found() {
        let p = lookup(1_000_059_001).unwrap();
        assert_eq!((p.name, p.size, p.align), ("VkPhysicalDeviceProperties2", 840, 8));
        assert_eq!(lookup(1).unwrap().name, "VkInstanceCreateInfo");
        assert_eq!(lookup(0).unwrap().name, "VkApplicationInfo");
    }

    #[test]
    fn unknown_values_are_none_not_panic() {
        for v in [u32::MAX, 0x7FFF_FFFF, 12345, 999_999_999] {
            assert!(lookup(v).is_none(), "{v}");
        }
    }

    /// Cały łańcuch (parser -> układ -> tablica) względem rozmiarów z kompilatora C.
    #[test]
    fn every_size_matches_the_c_compiler_golden_file() {
        let golden = include_str!("../../gpu-vkgen/tests/data/layout-golden.tsv");
        let mut sizes: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
        for line in golden.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f[0] == "S" {
                sizes.insert(f[1], (f[2].parse().unwrap(), f[3].parse().unwrap()));
            }
        }
        let mut checked = 0;
        for e in STYPE_TABLE {
            if let Some(&(size, align)) = sizes.get(e.name) {
                assert_eq!((e.size, e.align), (size, align), "{}", e.name);
                checked += 1;
            }
        }
        assert!(checked > 900, "sprawdzono tylko {checked} struktur");
    }
}
