//! Budowanie struktur Vulkana w układzie C na podstawie metadanych (po nazwach członków), do wołania sterownika przez thunk.
#![allow(dead_code)]

use gpu_proto::meta::{find_struct, StructMeta};
use gpu_proto::stype::STYPE_TABLE;

pub struct S {
    meta: &'static StructMeta,
    w: Vec<u64>,
}

impl S {
    /// Zerowa struktura o nazwie `name`, z ustawionym `sType` (jeśli struktura go ma).
    pub fn new(name: &str) -> S {
        let meta = find_struct(name).unwrap_or_else(|| panic!("brak struktury {name} w metadanych"));
        let mut s = S { meta, w: vec![0; (meta.size as usize).div_ceil(8).max(1)] };
        if let Some(e) = STYPE_TABLE.iter().find(|e| e.name == name) {
            s.u32("sType", e.stype);
        }
        s
    }

    fn off(&self, m: &str) -> usize {
        self.meta.members().iter().find(|x| x.name == m).unwrap_or_else(|| panic!("brak członka {}.{m}", self.meta.name)).offset as usize
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: bufor `u64` jako bajty, długość 8 * liczba słów.
        unsafe { std::slice::from_raw_parts_mut(self.w.as_mut_ptr() as *mut u8, self.w.len() * 8) }
    }

    pub fn u32(&mut self, m: &str, v: u32) -> &mut S {
        let o = self.off(m);
        self.bytes()[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, m: &str, v: u64) -> &mut S {
        let o = self.off(m);
        self.bytes()[o..o + 8].copy_from_slice(&v.to_le_bytes());
        self
    }

    /// Pole zagnieżdżonej struktury: `member` + przesunięcie wewnątrz niej.
    pub fn u32o(&mut self, m: &str, extra: usize, v: u32) -> &mut S {
        let o = self.off(m) + extra;
        self.bytes()[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self
    }

    pub fn read_u32(&self, m: &str) -> u32 {
        let o = self.off(m);
        // SAFETY: czytamy z własnego bufora w granicach struktury.
        let b = unsafe { std::slice::from_raw_parts(self.w.as_ptr() as *const u8, self.w.len() * 8) };
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    /// Adres struktury (do przekazania sterownikowi); żyje, dopóki żyje `S`.
    pub fn p(&self) -> u64 {
        self.w.as_ptr() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_structures_by_member_name_with_stype() {
        let mut s = S::new("VkBufferCreateInfo");
        s.u64("size", 4096).u32("usage", 3);
        assert_eq!(s.read_u32("sType"), 12, "VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO");
        assert_eq!(s.read_u32("usage"), 3);
        let mut c = S::new("VkBufferImageCopy");
        c.u32o("imageExtent", 4, 480);
        assert_eq!(c.read_u32("imageExtent") , 0);
    }

    #[test]
    #[should_panic(expected = "brak członka")]
    fn unknown_member_panics_loudly_in_the_builder() {
        S::new("VkBufferCreateInfo").u32("nieMaTakiego", 1);
    }
}
