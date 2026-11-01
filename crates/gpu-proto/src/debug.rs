//! Pomocnicze opisy struktur Vulkana do diagnostyki (czytane z surowej pamięci aplikacji lub zdekodowanej areny serwera).

/// Krótki opis `VkGraphicsPipelineCreateInfo` pod `p`: łańcuch `pNext` (typy), liczba stopni oraz które wskaźniki są niezerowe.
///
/// # Safety
/// `p` musi wskazywać na poprawną strukturę `VkGraphicsPipelineCreateInfo` (136 B) z łańcuchem `pNext` złożonym z poprawnych węzłów.
pub unsafe fn describe_graphics_pipeline_info(p: *const u8) -> String {
    let rd32 = |o: usize| std::ptr::read_unaligned(p.add(o) as *const u32);
    let rd64 = |o: usize| std::ptr::read_unaligned(p.add(o) as *const u64);
    let mut chain = Vec::new();
    let mut n = rd64(8) as *const u8;
    while !n.is_null() && chain.len() < 16 {
        chain.push(std::ptr::read_unaligned(n as *const u32));
        n = std::ptr::read_unaligned(n.add(8) as *const u64) as *const u8;
    }
    let names = ["stages", "vertexInput", "inputAssembly", "tessellation", "viewport", "raster", "multisample", "depthStencil", "colorBlend", "dynamic"];
    let offs = [24usize, 32, 40, 48, 56, 64, 72, 80, 88, 96];
    let set: Vec<&str> = names.iter().zip(offs).filter(|(_, o)| rd64(*o) != 0).map(|(n, _)| *n).collect();
    format!("flags={:#x} stageCount={} ptrs={:?} layout={:#x} renderPass={:#x} subpass={} pNext sTypes={:?}", rd32(16), rd32(20), set, rd64(104), rd64(112), rd32(120), chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_chain_and_non_null_pointers() {
        let mut node = [0u64; 2];
        node[0] = 1000044002; // sType w młodszych 32 bitach
        let mut info = [0u64; 17];
        info[2] = 0x7 | (3 << 32); // flags=7, stageCount=3
        info[1] = node.as_ptr() as u64;
        info[3] = 0x1000; // pStages
        info[11] = 0x2000; // pViewportState jest @56 => indeks 7; tu pDynamicState @96 => indeks 12
        info[12] = 0x3000;
        info[13] = 0xAA; // layout @104
        let d = unsafe { describe_graphics_pipeline_info(info.as_ptr() as *const u8) };
        assert!(d.contains("stageCount=3") && d.contains("flags=0x7"), "{d}");
        assert!(d.contains("\"stages\"") && d.contains("\"dynamic\""), "{d}");
        assert!(d.contains("1000044002"), "{d}");
        assert!(d.contains("layout=0xaa"), "{d}");
    }
}
