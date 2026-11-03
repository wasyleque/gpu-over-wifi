//! Obsługa unii `VkDescriptorDataEXT` (argument `vkGetDescriptorEXT`) w kodeku poleceń: wybór wariantu typem deskryptora,
//! tłumaczenie uchwytów, wartości puste i odrzucanie typów spoza zakresu.

use gpu_proto::meta::find_struct;
use gpu_vkcodec::codec::{decode_struct, encode_struct, Ctx, Decoded};
use gpu_vkcodec::CodecError;
use gpu_wire::{Decoder, Encoder};

/// Kontekst przesuwający uchwyty (klient -> protokół +1000 i z powrotem), żeby test widział, że tłumaczenie zachodzi.
struct Shift;
impl Ctx for Shift {
    fn handle_to_wire(&self, _ty: &str, raw: u64) -> Result<u64, CodecError> {
        Ok(if raw == 0 { 0 } else { raw + 1000 })
    }
    fn wire_to_handle(&self, _ty: &str, wire: u64) -> Result<u64, CodecError> {
        Ok(if wire == 0 { 0 } else { wire - 1000 })
    }
}

unsafe fn roundtrip_descriptor(ty: u32, data: u64) -> Result<(Decoded, Vec<u8>), CodecError> {
    let meta = find_struct("VkDescriptorGetInfoEXT").unwrap();
    let mut w = vec![0u64; meta.size as usize / 8];
    let b = w.as_mut_ptr() as *mut u8;
    let st = gpu_proto::stype::STYPE_TABLE.iter().find(|e| e.name == "VkDescriptorGetInfoEXT").unwrap().stype;
    std::ptr::write_unaligned(b as *mut u32, st);
    std::ptr::write_unaligned(b.add(16) as *mut u32, ty);
    std::ptr::write_unaligned(b.add(24) as *mut u64, data);
    let mut enc = Encoder::new();
    encode_struct(meta, b as *const u8, &mut enc, &Shift)?;
    let bytes = enc.finish();
    let dec = decode_struct(meta, &mut Decoder::new(&bytes), &Shift)?;
    Ok((dec, bytes))
}

#[test]
fn descriptor_data_union_roundtrips_every_variant_with_handle_translation() {
    unsafe {
        let rd = |d: &Decoded, off: usize| std::ptr::read_unaligned(d.ptr.add(off) as *const u64);
        // SAMPLER: wskaźnik na VkSampler.
        let sampler: u64 = 77;
        let (d, _) = roundtrip_descriptor(0, &sampler as *const u64 as u64).unwrap();
        assert_eq!(std::ptr::read_unaligned(rd(&d, 24) as *const u64), 77, "uchwyt przetłumaczony tam i z powrotem");
        // COMBINED_IMAGE_SAMPLER: obie wartości uchwytów zachowane.
        let info: [u64; 3] = [5, 9, 0x0000_0000_0000_0003];
        let (d, _) = roundtrip_descriptor(1, info.as_ptr() as u64).unwrap();
        let p = rd(&d, 24) as *const u64;
        assert_eq!((*p, *p.add(1), *p.add(2) as u32), (5, 9, 3));
        // SAMPLED_IMAGE / STORAGE_IMAGE / INPUT_ATTACHMENT: sampler ignorowany (nawet śmieci), widok i układ zachowane.
        for ty in [2u32, 3, 10] {
            let junk: [u64; 3] = [0xDEAD_BEEF, 42, 5];
            let (d, bytes) = roundtrip_descriptor(ty, junk.as_ptr() as u64).unwrap();
            let p = rd(&d, 24) as *const u64;
            assert_eq!((*p, *p.add(1), *p.add(2) as u32), (0, 42, 5), "typ {ty}: sampler wyzerowany");
            assert!(!bytes.windows(8).any(|w| w == 0xDEAD_BEEFu64.to_le_bytes()), "śmieci nie wychodzą na łącze");
        }
        // Bufory: VkDescriptorAddressInfoEXT (address, range, format) bez zmian.
        let mut addr = vec![0u64; 5];
        addr[0] = gpu_proto::stype::STYPE_TABLE.iter().find(|e| e.name == "VkDescriptorAddressInfoEXT").unwrap().stype as u64;
        addr[2] = 0x7000_0000_1000;
        addr[3] = 4096;
        addr[4] = 37;
        for ty in [4u32, 5, 6, 7] {
            let (d, _) = roundtrip_descriptor(ty, addr.as_ptr() as u64).unwrap();
            let p = rd(&d, 24) as *const u64;
            assert_eq!((*p.add(2), *p.add(3), *p.add(4) as u32), (0x7000_0000_1000, 4096, 37), "typ {ty}");
        }
        // Pusty wskaźnik zostaje pusty; struktura przyspieszenia to wartość u64.
        let (d, _) = roundtrip_descriptor(7, 0).unwrap();
        assert_eq!(rd(&d, 24), 0);
        let (d, _) = roundtrip_descriptor(1000150000, 0xABCD_EF01).unwrap();
        assert_eq!(rd(&d, 24), 0xABCD_EF01);
        // Typy spoza vkGetDescriptorEXT (np. dynamiczny bufor, 8) są odrzucane zamiast cicho wysyłać zera.
        assert!(roundtrip_descriptor(8, 0).is_err());
    }
}
