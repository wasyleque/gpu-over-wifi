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
