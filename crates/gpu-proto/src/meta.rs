//! Metadane struktur Vulkana (generowane z `vk.xml` do `meta_table.rs`): opis każdego członka potrzebny
//! interpreterowi kodeka. Układ pamięci: x86_64 Linux (zgodny z `layout-golden.tsv`).

pub use crate::meta_table::{CMD_MEMBERS, COMMANDS, MEMBERS, STRUCTS};

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
    /// Syntetyczna struktura parametrów polecenia: członkowie leżą w `CMD_MEMBERS`.
    pub commands: bool,
    /// Indeks pierwszego członka (w `MEMBERS` albo `CMD_MEMBERS`) i liczba członków.
    pub first: u32,
    pub count: u32,
}

impl StructMeta {
    pub fn members(&self) -> &'static [MemberMeta] {
        let all = if self.commands { CMD_MEMBERS } else { MEMBERS };
        &all[self.first as usize..(self.first + self.count) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ret {
    Void,
    VkResult,
    /// Wartość 64-bitowa (np. `VkDeviceAddress`), przesyłana po kodzie wyniku i parametrach wyjściowych.
    U64,
    Other,
}

/// Jak polecenie zachowuje się w sieci (klasyfikacja z `gpu-vkgen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdClass {
    FireAndForget,
    DeferredResult,
    OptimisticCreate,
    Query,
    Sync,
}

/// Polecenie Vulkana; parametry to członkowie syntetycznej struktury (slot 8 B na parametr).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandMeta {
    pub name: &'static str,
    pub ret: Ret,
    pub class: CmdClass,
    /// Kodek i serwer potrafią przenieść wszystkie parametry.
    pub supported: bool,
    pub first: u32,
    pub count: u32,
}

impl CommandMeta {
    pub fn params(&self) -> &'static [MemberMeta] {
        &CMD_MEMBERS[self.first as usize..(self.first + self.count) as usize]
    }

    /// Syntetyczna struktura parametrów (do kodeka): rozmiar to 8 B na parametr.
    pub fn as_struct(&self) -> StructMeta {
        StructMeta { name: self.name, size: self.count * 8, align: 8, is_union: false, commands: true, first: self.first, count: self.count }
    }
}

/// Wyszukuje polecenie po nazwie; zwraca też kod operacji (indeks).
pub fn find_command(name: &str) -> Option<(u16, &'static CommandMeta)> {
    COMMANDS.binary_search_by(|c| c.name.cmp(name)).ok().map(|i| (i as u16, &COMMANDS[i]))
}

/// Wyszukuje strukturę po nazwie (tablica `STRUCTS` jest posortowana po nazwie).
pub fn find_struct(name: &str) -> Option<&'static StructMeta> {
    STRUCTS.binary_search_by(|s| s.name.cmp(name)).ok().map(|i| &STRUCTS[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_returning_a_device_address_are_synchronous_and_supported() {
        for n in ["vkGetBufferDeviceAddress", "vkGetBufferOpaqueCaptureAddress", "vkGetDeviceMemoryOpaqueCaptureAddress"] {
            let (_, c) = find_command(n).unwrap_or_else(|| panic!("brak {n}"));
            assert_eq!((c.ret, c.class, c.supported), (Ret::U64, CmdClass::Query, true), "{n}");
        }
    }
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

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn commands_are_sorted_with_contiguous_param_ranges() {
        assert!(COMMANDS.windows(2).all(|w| w[0].name < w[1].name));
        let mut next = 0u32;
        for c in COMMANDS {
            assert_eq!(c.first, next, "{}", c.name);
            next += c.count;
        }
        assert_eq!(next as usize, CMD_MEMBERS.len());
        assert!(COMMANDS.len() < u16::MAX as usize, "kod operacji mieści się w u16");
    }

    #[test]
    fn find_command_gives_a_stable_opcode_and_params() {
        let (op, c) = find_command("vkQueueSubmit").unwrap();
        assert_eq!(COMMANDS[op as usize].name, "vkQueueSubmit");
        assert_eq!((c.class, c.ret, c.supported), (CmdClass::DeferredResult, Ret::VkResult, true));
        assert_eq!(c.params().len(), 4);
        assert_eq!(c.as_struct().size, 32);
        assert!(find_command("vkNieMaTakiego").is_none());
    }
}
