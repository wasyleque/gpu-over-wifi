//! Audyt: polecenia oznaczone jako obsługiwane, których parametry kodek poleceń nie umie zakodować (patrz `unencodable_reason`).
//! Wypisuje listę (`cargo test -p gpu-proto --test audit_unions -- --nocapture`); sterownik pilnuje, żeby żadne z nich nie trafiło
//! do ogłaszanych rozszerzeń (test w `gpu-icd/src/exts.rs`).

use gpu_proto::meta::{unencodable_reason, COMMANDS};

#[test]
fn list_supported_commands_that_the_codec_cannot_encode() {
    let found: Vec<String> = COMMANDS.iter().filter(|c| c.supported).filter_map(|c| unencodable_reason(c).map(|why| format!("{} ({why})", c.name))).collect();
    eprintln!("AUDYT: {} poleceń oznaczonych jako obsługiwane nie da się zakodować:", found.len());
    for f in &found {
        eprintln!("  {f}");
    }
    // Znane i zablokowane w ICD; wzrost tej liczby oznacza nowe rozszerzenie do sprawdzenia.
    assert!(found.len() <= 30, "nowe niekodowalne polecenia: {found:?}");
}

#[test]
fn the_check_itself_detects_the_known_bad_and_accepts_the_known_good_commands() {
    // Unia opisów deskryptorów ma własne kodowanie (patrz `gpu-vkcodec/tests/descriptor_data.rs`), więc `vkGetDescriptorEXT` jest kodowalne.
    let bad = gpu_proto::meta::find_command("vkCreateIndirectCommandsLayoutEXT").unwrap().1;
    assert!(unencodable_reason(bad).is_some_and(|r| r.contains("VkIndirectCommandsTokenDataEXT")));
    // Unie adres-urządzenia-albo-wskaźnik-hosta (ray tracing) są przenoszone jako 8 bajtów (patrz `RAW_ADDRESS_UNIONS`).
    for raw in ["vkCmdBuildAccelerationStructuresKHR", "vkGetAccelerationStructureBuildSizesKHR", "vkCmdCopyMemoryToAccelerationStructureKHR"] {
        assert_eq!(unencodable_reason(gpu_proto::meta::find_command(raw).unwrap().1), None, "{raw}");
    }
    for ok in ["vkGetDescriptorEXT", "vkCmdBindDescriptorBuffersEXT", "vkCreateBuffer", "vkQueueSubmit", "vkCreateGraphicsPipelines", "vkUpdateDescriptorSets", "vkCmdPipelineBarrier2"] {
        assert_eq!(unencodable_reason(gpu_proto::meta::find_command(ok).unwrap().1), None, "{ok}");
    }
}
