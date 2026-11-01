//! Generator tablicy `sType -> (nazwa, rozmiar, wyrównanie)` w postaci pliku źródłowego Rust.
#![allow(dead_code)]

use std::collections::BTreeMap;

use crate::layout::compute_layouts;
use crate::stype::compute_stypes;
use crate::structs::parse_structs;

/// Zwraca zawartość pliku `.rs` z tablicą posortowaną rosnąco po `sType` (bez duplikatów).
/// Aliasy struktur są pomijane: dla danego `sType` zostaje struktura kanoniczna.
pub fn stype_table_source(xml: &str) -> Result<String, String> {
    let structs = parse_structs(xml)?;
    let stypes = compute_stypes(xml)?;
    let layouts = compute_layouts(xml)?;
    let mut rows: BTreeMap<u32, (String, usize, usize, bool)> = BTreeMap::new();
    for s in structs.iter().filter(|s| !s.is_union && s.alias_of.is_none()) {
        let Some(first) = s.members.first().filter(|m| m.name == "sType") else { continue };
        let Some(values) = &first.values else { continue };
        let Some(layout) = layouts.ok.get(&s.name) else { continue };
        for v in values.split(',') {
            if let Some(&num) = stypes.get(v.trim()) {
                let out_ptr = s.members.iter().any(|m| m.name != "pNext" && m.ptr_depth > 0);
                rows.entry(num).or_insert((s.name.clone(), layout.size, layout.align, out_ptr));
            }
        }
    }
    let mut out = String::from(
        "//! GENEROWANY przez `gpu-vkgen --emit-stype-table`. Nie edytować ręcznie.\n\n\
         /// Wpis tablicy: numer `sType`, nazwa struktury, jej rozmiar i wyrównanie (x86_64 Linux).\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         pub struct StypeEntry {\n    pub stype: u32,\n    pub name: &'static str,\n    pub size: u32,\n    pub align: u32,\n    /// Struktura ma członka-wskaźnika poza `pNext` (nie wolno jej wystawiać klientowi jako wyjście).\n    pub has_output_pointers: bool,\n}\n\n\
         /// Posortowana rosnąco po `stype`.\n\
         pub const STYPE_TABLE: &[StypeEntry] = &[\n",
    );
    for (stype, (name, size, align, out_ptr)) in &rows {
        out.push_str(&format!(
            "    StypeEntry {{ stype: {stype}, name: \"{name}\", size: {size}, align: {align}, has_output_pointers: {out_ptr} }},\n"
        ));
    }
    out.push_str("];\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_sorted_unique_rows_for_structs_with_stype() {
        let xml = r#"<registry>
          <types>
            <type requires="vk_platform" name="uint32_t"/>
            <type category="enum" name="VkStructureType"/>
            <type category="basetype">typedef <type>uint32_t</type> <name>VkFlags</name>;</type>
            <type category="struct" name="VkB"><member values="VK_STRUCTURE_TYPE_B"><type>VkStructureType</type> <name>sType</name></member><member><type>uint32_t</type> <name>x</name></member></type>
            <type category="struct" name="VkA"><member values="VK_STRUCTURE_TYPE_A"><type>VkStructureType</type> <name>sType</name></member></type>
            <type category="struct" name="VkAAlias" alias="VkA"/>
            <type category="struct" name="VkNoType"><member><type>uint32_t</type> <name>x</name></member></type>
          </types>
          <enums name="VkStructureType" type="enum">
            <enum value="5" name="VK_STRUCTURE_TYPE_A"/>
            <enum value="2" name="VK_STRUCTURE_TYPE_B"/>
          </enums>
        </registry>"#;
        let src = stype_table_source(xml).unwrap();
        let b = src.find("stype: 2,").expect("wiersz B");
        let a = src.find("stype: 5,").expect("wiersz A");
        assert!(b < a, "posortowane rosnąco");
        assert!(src.contains("name: \"VkB\", size: 8, align: 4, has_output_pointers: false"));
        assert!(src.contains("name: \"VkA\", size: 4, align: 4"));
        assert!(!src.contains("VkAAlias") && !src.contains("VkNoType"));
    }

    #[test]
    fn real_registry_has_expected_core_rows() {
        let xml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/vk.xml")).unwrap();
        let src = stype_table_source(&xml).unwrap();
        assert!(src.contains("StypeEntry { stype: 1000059001, name: \"VkPhysicalDeviceProperties2\""), "Properties2");
        assert!(src.contains("StypeEntry { stype: 1, name: \"VkInstanceCreateInfo\", size: 64, align: 8, has_output_pointers: true }"), "InstanceCreateInfo ma wskaźniki");
        assert!(src.matches("StypeEntry {").count() > 900, "za mało wierszy");
    }
}

/// Tablica rozszerzeń urządzenia: nazwa i polecenia, które rozszerzenie wprowadza (ze wszystkich bloków `require`).
/// Rozszerzenie bez poleceń ma pustą listę. Tylko API `vulkan`, tylko `type="device"`.
pub fn ext_table_source(xml: &str) -> Result<(String, usize), String> {
    let doc = roxmltree::Document::parse_with_options(xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).map_err(|e| e.to_string())?;
    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    for e in doc.descendants().filter(|n| n.is_element() && n.has_tag_name("extension")) {
        if e.attribute("type") != Some("device") || !e.attribute("supported").is_some_and(|s| s.split(',').any(|t| t.trim() == "vulkan")) {
            continue;
        }
        let Some(name) = e.attribute("name") else { continue };
        let mut cmds: Vec<String> = e
            .descendants()
            .filter(|n| n.is_element() && n.has_tag_name("command"))
            .filter(|n| n.ancestors().any(|a| a.has_tag_name("require")))
            .filter_map(|n| n.attribute("name").map(str::to_string))
            .collect();
        cmds.sort();
        cmds.dedup();
        rows.push((name.to_string(), cmds));
    }
    rows.sort();
    let mut src = String::from("//! GENEROWANY przez `gpu-vkgen --emit-ext-table`. Nie edytować ręcznie.\n\n/// Rozszerzenia urządzenia i polecenia, które wprowadzają.\npub static DEVICE_EXTENSIONS: &[(&str, &[&str])] = &[\n");
    for (n, c) in &rows {
        src.push_str(&format!("    (\"{n}\", &["));
        src.push_str(&c.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(", "));
        src.push_str("]),\n");
    }
    src.push_str("];\n\n/// Aliasy poleceń (np. `vkTrimCommandPoolKHR` -> `vkTrimCommandPool`), posortowane po aliasie.\npub static COMMAND_ALIASES: &[(&str, &str)] = &[\n");
    let mut aliases: Vec<(String, String)> = doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("command") && n.attribute("alias").is_some() && n.parent().is_some_and(|p| p.has_tag_name("commands")))
        .filter(|n| n.attribute("api").is_none_or(|a| a.split(',').any(|t| t.trim() == "vulkan")))
        .filter_map(|n| Some((n.attribute("name")?.to_string(), n.attribute("alias")?.to_string())))
        .collect();
    aliases.sort();
    aliases.dedup();
    for (a, t) in &aliases {
        src.push_str(&format!("    (\"{a}\", \"{t}\"),\n"));
    }
    src.push_str("];\n");
    Ok((src, rows.len()))
}

#[cfg(test)]
mod ext_tests {
    use super::*;

    #[test]
    fn extension_table_lists_device_extensions_with_their_commands() {
        let xml = r#"<registry><extensions>
          <extension name="VK_KHR_a" number="1" type="device" supported="vulkan"><require><command name="vkA"/><command name="vkB"/></require><require><command name="vkA"/></require></extension>
          <extension name="VK_KHR_inst" number="2" type="instance" supported="vulkan"><require><command name="vkI"/></require></extension>
          <extension name="VK_KHR_disabled" number="3" type="device" supported="disabled"><require><command name="vkD"/></require></extension>
          <extension name="VK_KHR_none" number="4" type="device" supported="vulkan,vulkansc"><require><type name="X"/></require></extension>
        </extensions></registry>"#;
        let (src, n) = ext_table_source(xml).unwrap();
        assert_eq!(n, 2);
        assert!(src.contains("(\"VK_KHR_a\", &[\"vkA\", \"vkB\"]),"), "{src}");
        assert!(src.contains("(\"VK_KHR_none\", &[]),"));
        assert!(!src.contains("VK_KHR_inst") && !src.contains("disabled"));
    }
}
