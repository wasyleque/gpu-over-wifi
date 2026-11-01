//! Metadane struktur Vulkana (generowane z `vk.xml` do `meta_table.rs`): opis każdego członka potrzebny
//! interpreterowi kodeka. Układ pamięci: x86_64 Linux (zgodny z `layout-golden.tsv`).

pub use crate::meta_table::{MEMBERS, STRUCTS};

/// Kształt członka.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Wartość wbudowana (skalar, uchwyt, struktura, unia lub tablica o stałej liczbie elementów).
    Value,
    /// Wskaźnik (`ptr_depth` 1 lub 2) na elementy typu `elem`.
    Pointer,
    /// `void* pNext`: łańcuch struktur rozszerzających.
    PNext,
    /// Pole bitowe (nieobsługiwane w kodeku M2).
    Bitfield,
}

/// Rodzaj elementu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elem {
    /// Liczba całkowita lub zmiennoprzecinkowa (także enum i flagi); rozmiar w `elem_size`.
    Scalar,
    /// Uchwyt obiektu Vulkana (8 B), tłumaczony między klientem a serwerem.
    Handle,
    /// Struktura (nazwa w `type_name`), rozmiar w `elem_size`.
    Struct,
    /// Unia (nazwa w `type_name`), rozmiar w `elem_size`.
    Union,
    /// `void` (wskaźnik na dane o długości z `len`).
    Void,
    /// `char` (napisy).
    Char,
    /// Wskaźnik na funkcję (PFN_*): nie przekazywany przez sieć.
    FuncPtr,
    /// Typ platformowy spoza Vulkana (xcb, wayland...): nieprzejrzysty.
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberMeta {
    pub name: &'static str,
    pub offset: u32,
    pub shape: Shape,
    pub elem: Elem,
    /// Nazwa typu elementu w C (np. `VkApplicationInfo`, `uint32_t`).
    pub type_name: &'static str,
    /// Rozmiar jednego elementu w bajtach (0 dla `Void`).
    pub elem_size: u32,
    /// Liczba elementów tablicy stałej (1 dla pojedynczej wartości).
    pub count: u32,
    pub ptr_depth: u8,
    pub is_const: bool,
    pub optional: bool,
    /// Wyrażenie długości z `vk.xml` (pusty, gdy brak), np. `enabledLayerCount,null-terminated`.
    pub len: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructMeta {
    pub name: &'static str,
    pub size: u32,
    pub align: u32,
    pub is_union: bool,
    /// Indeks pierwszego członka w `MEMBERS` i liczba członków.
    pub first: u32,
    pub count: u32,
}

impl StructMeta {
    pub fn members(&self) -> &'static [MemberMeta] {
        &MEMBERS[self.first as usize..(self.first + self.count) as usize]
    }
}

/// Wyszukuje strukturę po nazwie (tablica `STRUCTS` jest posortowana po nazwie).
pub fn find_struct(name: &str) -> Option<&'static StructMeta> {
    STRUCTS.binary_search_by(|s| s.name.cmp(name)).ok().map(|i| &STRUCTS[i])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stype::STYPE_TABLE;

    #[test]
    fn table_is_sorted_and_member_ranges_are_contiguous() {
        assert!(STRUCTS.windows(2).all(|w| w[0].name < w[1].name));
        let mut next = 0u32;
        for s in STRUCTS {
            assert_eq!(s.first, next, "{}", s.name);
            next += s.count;
        }
        assert_eq!(next as usize, MEMBERS.len());
    }

    #[test]
    fn sizes_agree_with_the_stype_table_for_every_struct_with_stype() {
        let mut checked = 0;
        for e in STYPE_TABLE {
            let m = find_struct(e.name).unwrap_or_else(|| panic!("brak metadanych {}", e.name));
            assert_eq!((m.size, m.align), (e.size, e.align), "{}", e.name);
            checked += 1;
        }
        assert!(checked > 1000);
    }

    #[test]
    fn has_output_pointers_agrees_with_member_shapes() {
        for e in STYPE_TABLE {
            let m = find_struct(e.name).unwrap();
            let ptr = m.members().iter().any(|x| x.name != "pNext" && x.ptr_depth > 0);
            assert_eq!(ptr, e.has_output_pointers, "{}", e.name);
        }
    }

    #[test]
    fn lookup_and_members_work_for_known_structs() {
        let s = find_struct("VkInstanceCreateInfo").unwrap();
        assert_eq!(s.members().len(), 8);
        assert_eq!(s.members()[1].shape, Shape::PNext);
        assert!(find_struct("VkNieMaTakiej").is_none());
    }
}
