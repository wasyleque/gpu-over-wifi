//! `vkCmdBuildAccelerationStructuresKHR`: `ppBuildRangeInfos[i]` ma `pInfos[i].geometryCount` elementów (tablica wskaźników do tablic),
//! a unie adres-urządzenia-albo-wskaźnik-hosta przechodzą jako 8 bajtów.

use gpu_proto::meta::{find_command, find_struct};
use gpu_vkcodec::codec::{decode_command, encode_command, Identity};
use gpu_wire::{Decoder, Encoder};

#[test]
fn per_info_range_arrays_roundtrip_with_geometry_counts_taken_from_pinfos() {
    unsafe {
        let (op, cmd) = find_command("vkCmdBuildAccelerationStructuresKHR").unwrap();
        assert!(cmd.supported);
        let gm = find_struct("VkAccelerationStructureBuildGeometryInfoKHR").unwrap();
        let gc = gm.members().iter().find(|m| m.name == "geometryCount").unwrap().offset as usize;
        let st = gpu_proto::stype::STYPE_TABLE.iter().find(|e| e.name == "VkAccelerationStructureBuildGeometryInfoKHR").unwrap().stype;
        // Dwa elementy pInfos: geometryCount 2 i 1 (pGeometries puste: ten test dotyczy tylko tablic zakresów).
        let mut infos = vec![0u8; gm.size as usize * 2];
        for (i, n) in [2u32, 1].iter().enumerate() {
            std::ptr::write_unaligned(infos.as_mut_ptr().add(i * gm.size as usize) as *mut u32, st);
            std::ptr::write_unaligned(infos.as_mut_ptr().add(i * gm.size as usize + gc) as *mut u32, *n);
        }
        let r0: [[u32; 4]; 2] = [[10, 0, 0, 0], [20, 64, 0, 0]];
        let r1: [[u32; 4]; 1] = [[30, 128, 4, 0]];
        let rows: [*const u8; 2] = [r0.as_ptr() as *const u8, r1.as_ptr() as *const u8];
        let args: [u64; 4] = [0xC0FFEE, 2, infos.as_ptr() as u64, rows.as_ptr() as u64];
        let mut e = Encoder::new();
        encode_command(op, args.as_ptr() as *const u8, &mut e, &Identity).unwrap();
        let bytes = e.finish();
        let dc = decode_command(&mut Decoder::new(&bytes), &Identity).unwrap().unwrap();
        let slots = dc.args.ptr as *const u64;
        assert_eq!(std::ptr::read_unaligned(slots.add(1)), 2);
        let pp = std::ptr::read_unaligned(slots.add(3)) as *const *const [u32; 4];
        let (a, b) = (std::ptr::read_unaligned(pp), std::ptr::read_unaligned(pp.add(1)));
        assert_eq!((*a, *a.add(1), *b), (r0[0], r0[1], r1[0]));
        // Wskaźniki są nowe (odtworzone w arenie dekodera), nie adresami nadawcy.
        assert_ne!(a as u64, r0.as_ptr() as u64);
    }
}

#[test]
fn a_range_array_whose_length_disagrees_with_geometry_count_is_rejected_when_decoding() {
    unsafe {
        let (op, _) = find_command("vkCmdBuildAccelerationStructuresKHR").unwrap();
        let gm = find_struct("VkAccelerationStructureBuildGeometryInfoKHR").unwrap();
        let gc = gm.members().iter().find(|m| m.name == "geometryCount").unwrap().offset as usize;
        let st = gpu_proto::stype::STYPE_TABLE.iter().find(|e| e.name == "VkAccelerationStructureBuildGeometryInfoKHR").unwrap().stype;
        let mut infos = vec![0u8; gm.size as usize];
        std::ptr::write_unaligned(infos.as_mut_ptr() as *mut u32, st);
        std::ptr::write_unaligned(infos.as_mut_ptr().add(gc) as *mut u32, 1);
        let r0: [[u32; 4]; 1] = [[1, 2, 3, 4]];
        let rows: [*const u8; 1] = [r0.as_ptr() as *const u8];
        let args: [u64; 4] = [1, 1, infos.as_ptr() as u64, rows.as_ptr() as u64];
        let mut e = Encoder::new();
        encode_command(op, args.as_ptr() as *const u8, &mut e, &Identity).unwrap();
        let mut bytes = e.finish();
        // Podmieniamy w strumieniu liczbę elementów wiersza (1 -> 2): dekoder musi ją odrzucić, bo `pInfos[0].geometryCount` == 1.
        // Wiersz zaczyna się od varintu 1, po którym idą 4 słowa u32 danych (1, 2, 3, 4); szukamy tego wzorca od końca strumienia.
        // Każdy u32 jest zapisany jako bajty z prefiksem długości (4), więc wiersz to: 1, [4,1,0,0,0, 4,2,0,0,0, 4,3,0,0,0, 4,4,0,0,0].
        let tail: Vec<u8> = [1u32, 2, 3, 4].iter().flat_map(|v| [4u8].into_iter().chain(v.to_le_bytes())).collect();
        let at = (1..bytes.len()).rev().find(|&i| bytes[i..].starts_with(&tail) && bytes[i - 1] == 1).expect("wiersz w strumieniu");
        bytes[at - 1] = 2;
        assert!(decode_command(&mut Decoder::new(&bytes), &Identity).is_err());
    }
}
