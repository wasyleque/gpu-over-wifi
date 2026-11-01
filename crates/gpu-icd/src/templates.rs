//! `vkUpdateDescriptorSetWithTemplate`: dane (`pData`) mają układ opisany szablonem, a w środku leżą uchwyty (widoki obrazów,
//! próbniki, bufory), które trzeba przetłumaczyć na identyfikatory sieciowe. Kodek nie zna tego układu, więc ICD rozwija wywołanie
//! do zwykłego `vkUpdateDescriptorSets` z jawnymi strukturami `VkWriteDescriptorSet`.

const WRITE_DESCRIPTOR_SET: u32 = 35;
const IMAGE_INFO_SIZE: usize = 24;
const BUFFER_INFO_SIZE: usize = 24;
const TEXEL_VIEW_SIZE: usize = 8;

/// Jeden wpis szablonu (`VkDescriptorUpdateTemplateEntry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub dst_binding: u32,
    pub dst_array_element: u32,
    pub count: u32,
    pub descriptor_type: u32,
    pub offset: usize,
    pub stride: usize,
}

/// Rozmiar elementu w `pData` dla typu deskryptora i rodzaj tablicy w `VkWriteDescriptorSet` (0 = obraz, 1 = bufor, 2 = widok bufora).
fn element(descriptor_type: u32) -> Option<(usize, u8)> {
    match descriptor_type {
        0 | 1 | 2 | 3 | 10 => Some((IMAGE_INFO_SIZE, 0)),
        6..=9 => Some((BUFFER_INFO_SIZE, 1)),
        4 | 5 => Some((TEXEL_VIEW_SIZE, 2)),
        _ => None,
    }
}

/// Czyta wpisy z `VkDescriptorUpdateTemplateCreateInfo` (liczba @20, wskaźnik @24; wpis ma 32 B).
///
/// # Safety
/// `info` musi wskazywać na poprawną strukturę z tablicą wpisów o podanej długości.
pub unsafe fn parse_entries(info: *const u8) -> Vec<Entry> {
    let count = std::ptr::read_unaligned(info.add(20) as *const u32).min(4096) as usize;
    let ptr = std::ptr::read_unaligned(info.add(24) as *const u64) as *const u8;
    if ptr.is_null() {
        return Vec::new();
    }
    (0..count)
        .map(|i| {
            let e = ptr.add(i * 32);
            Entry {
                dst_binding: std::ptr::read_unaligned(e as *const u32),
                dst_array_element: std::ptr::read_unaligned(e.add(4) as *const u32),
                count: std::ptr::read_unaligned(e.add(8) as *const u32),
                descriptor_type: std::ptr::read_unaligned(e.add(12) as *const u32),
                offset: std::ptr::read_unaligned(e.add(16) as *const u64) as usize,
                stride: std::ptr::read_unaligned(e.add(24) as *const u64) as usize,
            }
        })
        .collect()
}

/// Gotowe struktury `VkWriteDescriptorSet` wraz z tablicami, na które wskazują.
pub struct Writes {
    pub bytes: Vec<u8>,
    pub count: u32,
    _keep: Vec<Vec<u8>>,
}

/// Rozwija dane szablonu do `VkWriteDescriptorSet` dla zestawu `set`. `None`, gdy wpis ma nieobsługiwany typ deskryptora
/// (np. inline uniform block) albo liczba elementów jest absurdalna.
///
/// # Safety
/// `data` musi wskazywać na pamięć aplikacji zgodną z układem opisanym przez `entries`.
pub unsafe fn build_writes(entries: &[Entry], data: *const u8, set: u64) -> Option<Writes> {
    let mut bytes = Vec::with_capacity(entries.len() * 64);
    let mut keep: Vec<Vec<u8>> = Vec::new();
    let mut count = 0u32;
    for e in entries {
        if e.count == 0 {
            continue;
        }
        let (size, kind) = element(e.descriptor_type)?;
        if e.count > 65536 {
            return None;
        }
        let mut arr = vec![0u8; size * e.count as usize];
        for k in 0..e.count as usize {
            let stride = if e.count == 1 { 0 } else { e.stride };
            let src = data.add(e.offset + k * stride);
            std::ptr::copy_nonoverlapping(src, arr.as_mut_ptr().add(k * size), size);
        }
        let mut w = [0u8; 64];
        w[0..4].copy_from_slice(&WRITE_DESCRIPTOR_SET.to_le_bytes());
        w[16..24].copy_from_slice(&set.to_le_bytes());
        w[24..28].copy_from_slice(&e.dst_binding.to_le_bytes());
        w[28..32].copy_from_slice(&e.dst_array_element.to_le_bytes());
        w[32..36].copy_from_slice(&e.count.to_le_bytes());
        w[36..40].copy_from_slice(&e.descriptor_type.to_le_bytes());
        let slot = match kind {
            0 => 40,
            1 => 48,
            _ => 56,
        };
        w[slot..slot + 8].copy_from_slice(&(arr.as_ptr() as u64).to_le_bytes());
        keep.push(arr);
        bytes.extend_from_slice(&w);
        count += 1;
    }
    Some(Writes { bytes, count, _keep: keep })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(binding: u32, count: u32, ty: u32, offset: usize, stride: usize) -> Entry {
        Entry { dst_binding: binding, dst_array_element: 0, count, descriptor_type: ty, offset, stride }
    }

    #[test]
    fn parses_template_entries_from_a_create_info() {
        let mut raw = [0u8; 64];
        raw[0..4].copy_from_slice(&1_000_085_000u32.to_le_bytes());
        let mut e = [0u8; 64];
        e[0..4].copy_from_slice(&3u32.to_le_bytes()); // binding
        e[8..12].copy_from_slice(&2u32.to_le_bytes()); // count
        e[12..16].copy_from_slice(&1u32.to_le_bytes()); // COMBINED_IMAGE_SAMPLER
        e[16..24].copy_from_slice(&16u64.to_le_bytes()); // offset
        e[24..32].copy_from_slice(&24u64.to_le_bytes()); // stride
        e[32 + 12..32 + 16].copy_from_slice(&6u32.to_le_bytes()); // drugi wpis: UNIFORM_BUFFER
        raw[20..24].copy_from_slice(&2u32.to_le_bytes());
        raw[24..32].copy_from_slice(&(e.as_ptr() as u64).to_le_bytes());
        let got = unsafe { parse_entries(raw.as_ptr()) };
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], entry(3, 2, 1, 16, 24));
        assert_eq!(got[1].descriptor_type, 6);
    }

    #[test]
    fn expands_image_buffer_and_texel_entries_into_write_structs_with_repacked_arrays() {
        // pData: [0..48) dwa obrazy co 24 B z przerwą (stride 32), potem bufor @100, potem widok bufora @200.
        let mut data = vec![0u8; 256];
        for (i, b) in data[0..24].iter_mut().enumerate() {
            *b = 0x10 + i as u8;
        }
        for (i, b) in data[32..56].iter_mut().enumerate() {
            *b = 0x40 + i as u8;
        }
        data[100] = 0x77;
        data[200] = 0x99;
        let entries = [entry(0, 2, 1, 0, 32), entry(1, 1, 6, 100, 0), entry(2, 1, 4, 200, 0)];
        let w = unsafe { build_writes(&entries, data.as_ptr(), 0xABC) }.unwrap();
        assert_eq!((w.count, w.bytes.len()), (3, 192));
        let rd32 = |o: usize| u32::from_le_bytes(w.bytes[o..o + 4].try_into().unwrap());
        let rd64 = |o: usize| u64::from_le_bytes(w.bytes[o..o + 8].try_into().unwrap());
        assert_eq!((rd32(0), rd64(16), rd32(24), rd32(32), rd32(36)), (35, 0xABC, 0, 2, 1));
        // Obrazy przepakowane w ciągłą tablicę 2 x 24 B.
        let img = unsafe { std::slice::from_raw_parts(rd64(40) as *const u8, 48) };
        assert_eq!(img[0], 0x10);
        assert_eq!(img[24], 0x40);
        assert_eq!(rd64(48), 0, "pBufferInfo puste dla obrazów");
        // Bufor w pBufferInfo (@48 drugiej struktury = 64 + 48), widok w pTexelBufferView (@56 trzeciej).
        assert_eq!(unsafe { *(rd64(64 + 48) as *const u8) }, 0x77);
        assert_eq!(unsafe { *(rd64(128 + 56) as *const u8) }, 0x99);
    }

    #[test]
    fn unsupported_types_and_absurd_counts_are_refused() {
        let data = [0u8; 64];
        assert!(unsafe { build_writes(&[entry(0, 1, 1_000_138_000, 0, 0)], data.as_ptr(), 1) }.is_none(), "inline uniform block");
        assert!(unsafe { build_writes(&[entry(0, 1 << 20, 6, 0, 24)], data.as_ptr(), 1) }.is_none());
        assert_eq!(unsafe { build_writes(&[entry(0, 0, 6, 0, 24)], data.as_ptr(), 1) }.unwrap().count, 0);
    }
}
