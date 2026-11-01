//! Wartości `VK_STRUCTURE_TYPE_*` z `vk.xml`: rdzeń (`value`) i rozszerzenia
//! (`1000000000 + (numer_rozszerzenia - 1) * 1000 + offset`), z aliasami.
#![allow(dead_code)]

use std::collections::BTreeMap;

/// Liczy wartości wszystkich stałych `VkStructureType` dostępnych w API `vulkan`.
pub fn compute_stypes(xml: &str) -> Result<BTreeMap<String, u32>, String> {
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() },
    )
    .map_err(|e| e.to_string())?;

    let mut values: BTreeMap<String, u32> = BTreeMap::new();
    let mut aliases: Vec<(String, String)> = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element() && n.has_tag_name("enum")) {
        let Some(name) = n.attribute("name") else { continue };
        let in_core = n
            .parent()
            .is_some_and(|p| p.has_tag_name("enums") && p.attribute("name") == Some("VkStructureType"));
        let extends = n.attribute("extends") == Some("VkStructureType");
        if !in_core && !extends {
            continue;
        }
        if n.attribute("api").is_some_and(|api| !api.split(',').any(|t| t.trim() == "vulkan")) {
            continue;
        }
        if let Some(target) = n.attribute("alias") {
            aliases.push((name.to_string(), target.to_string()));
            continue;
        }
        let value = if let Some(v) = n.attribute("value") {
            v.trim().parse::<u32>().ok()
        } else if let Some(offset) = n.attribute("offset").and_then(|o| o.trim().parse::<u32>().ok()) {
            let ext_no = n
                .attribute("extnumber")
                .and_then(|x| x.trim().parse::<u32>().ok())
                .or_else(|| {
                    n.ancestors()
                        .find(|a| a.has_tag_name("extension"))
                        .and_then(|e| e.attribute("number"))
                        .and_then(|x| x.trim().parse::<u32>().ok())
                });
            ext_no.and_then(|e| e.checked_sub(1)).map(|e| 1_000_000_000 + e * 1000 + offset)
        } else {
            None
        };
        if let Some(v) = value {
            values.insert(name.to_string(), v);
        }
    }
    // Aliasy mogą wskazywać na inne aliasy: powtarzamy, aż nic się nie zmienia.
    loop {
        let before = values.len();
        for (name, target) in &aliases {
            if !values.contains_key(name) {
                if let Some(v) = values.get(target).copied() {
                    values.insert(name.clone(), v);
                }
            }
        }
        if values.len() == before {
            break;
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<registry>
      <enums name="VkStructureType" type="enum">
        <enum value="0" name="VK_STRUCTURE_TYPE_APPLICATION_INFO"/>
        <enum value="1" name="VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO"/>
        <enum alias="VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO" name="VK_STRUCTURE_TYPE_INSTANCE_ALIAS"/>
      </enums>
      <extensions>
        <extension name="VK_KHR_foo" number="60">
          <require>
            <enum offset="1" extends="VkStructureType" name="VK_STRUCTURE_TYPE_FOO_KHR"/>
            <enum offset="2" extnumber="3" extends="VkStructureType" name="VK_STRUCTURE_TYPE_BAR_EXT"/>
            <enum extends="VkStructureType" name="VK_STRUCTURE_TYPE_FOO_ALIAS" alias="VK_STRUCTURE_TYPE_FOO_KHR"/>
            <enum api="vulkansc" offset="9" extends="VkStructureType" name="VK_STRUCTURE_TYPE_SC_ONLY"/>
            <enum offset="5" extends="VkFormat" name="VK_FORMAT_NOT_A_STYPE"/>
          </require>
        </extension>
      </extensions>
    </registry>"#;

    #[test]
    fn core_extension_extnumber_and_alias_values() {
        let v = compute_stypes(SAMPLE).unwrap();
        assert_eq!(v["VK_STRUCTURE_TYPE_APPLICATION_INFO"], 0);
        assert_eq!(v["VK_STRUCTURE_TYPE_INSTANCE_ALIAS"], 1);
        assert_eq!(v["VK_STRUCTURE_TYPE_FOO_KHR"], 1_000_059_001, "(60-1)*1000 + 1");
        assert_eq!(v["VK_STRUCTURE_TYPE_BAR_EXT"], 1_000_002_002, "extnumber ma pierwszeństwo przed numerem rozszerzenia");
        assert_eq!(v["VK_STRUCTURE_TYPE_FOO_ALIAS"], 1_000_059_001);
    }

    #[test]
    fn other_api_and_other_enums_are_ignored() {
        let v = compute_stypes(SAMPLE).unwrap();
        assert!(!v.contains_key("VK_STRUCTURE_TYPE_SC_ONLY"));
        assert!(!v.contains_key("VK_FORMAT_NOT_A_STYPE"));
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(compute_stypes("<registry>").is_err());
    }

    #[test]
    fn matches_real_c_compiler_for_every_constant() {
        let xml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/vk.xml")).unwrap();
        let v = compute_stypes(&xml).unwrap();
        let golden = include_str!("../tests/data/stype-golden.tsv");
        let mut errors = Vec::new();
        let mut n = 0;
        for line in golden.lines() {
            let mut f = line.split('\t');
            let (Some(name), Some(val)) = (f.next(), f.next()) else { continue };
            if name.ends_with("_MAX_ENUM") {
                continue; // wartownik 0x7FFFFFFF dopisywany przez nagłówek C, nie ma go w vk.xml
            }
            n += 1;
            let want: u32 = val.parse().unwrap();
            match v.get(name) {
                Some(got) if *got == want => {}
                other => errors.push(format!("{name}: jest {other:?}, wzorzec {want}")),
            }
        }
        assert!(n >= 1400, "wzorzec za mały: {n}");
        assert!(errors.is_empty(), "{} błędów, pierwsze: {:#?}", errors.len(), &errors[..errors.len().min(10)]);
    }
}
