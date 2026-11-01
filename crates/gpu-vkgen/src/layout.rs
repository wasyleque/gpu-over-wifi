//! Silnik układu pamięci struktur Vulkana (ABI x86_64 Linux, jak gcc): rozmiar, wyrównanie
//! i przesunięcia pól każdej struktury i unii z `vk.xml`. Podstawa generycznego kopiowania
//! struktur "tylko do odczytu" (właściwości, funkcje) bez kodu dla każdej z osobna.
#![allow(dead_code)]

use std::collections::BTreeMap;

/// Układ jednej struktury lub unii.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub size: usize,
    pub align: usize,
    /// Przesunięcia pól w kolejności deklaracji, BEZ pól bitowych.
    pub offsets: Vec<(String, usize)>,
}

/// Wynik obliczeń: struktury policzone i te, których nie dało się policzyć (z powodem).
#[derive(Debug, Default)]
pub struct Layouts {
    pub ok: BTreeMap<String, Layout>,
    pub failed: BTreeMap<String, String>,
}

/// Liczy układ wszystkich struktur i unii (aliasy dostają układ celu).
pub fn compute_layouts(xml: &str) -> Result<Layouts, String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimalny rejestr w formie takiej jak w prawdziwym vk.xml.
    const SAMPLE: &str = r#"<registry>
  <types>
    <type requires="vk_platform" name="uint8_t"/>
    <type requires="vk_platform" name="uint32_t"/>
    <type requires="vk_platform" name="uint64_t"/>
    <type requires="vk_platform" name="float"/>
    <type requires="vk_platform" name="double"/>
    <type requires="vk_platform" name="char"/>
    <type category="basetype">typedef <type>uint32_t</type> <name>VkFlags</name>;</type>
    <type category="basetype">typedef <type>uint64_t</type> <name>VkFlags64</name>;</type>
    <type category="basetype">typedef <type>uint32_t</type> <name>VkBool32</name>;</type>
    <type category="handle" objtypeenum="VK_OBJECT_TYPE_INSTANCE"><type>VK_DEFINE_HANDLE</type>(<name>VkInstance</name>)</type>
    <type category="handle" objtypeenum="VK_OBJECT_TYPE_BUFFER"><type>VK_DEFINE_NON_DISPATCHABLE_HANDLE</type>(<name>VkBuffer</name>)</type>
    <type category="funcpointer"><proto><type>void</type> <name>PFN_vkFoo</name></proto></type>
    <type requires="VkFooFlagBits" category="bitmask">typedef <type>VkFlags</type> <name>VkFooFlags</name>;</type>
    <type category="enum" name="VkFormat"/>
    <type category="struct" name="VkPad">
      <member><type>uint8_t</type> <name>a</name></member>
      <member><type>uint32_t</type> <name>b</name></member>
      <member><type>uint8_t</type> <name>c</name></member>
    </type>
    <type category="struct" name="VkInner"><member><type>uint8_t</type> <name>x</name></member></type>
    <type category="struct" name="VkOuter">
      <member><type>VkInner</type> <name>i</name></member>
      <member><type>double</type> <name>d</name></member>
    </type>
    <type category="struct" name="VkArrays">
      <member><type>char</type> <name>name</name>[<enum>VK_MAX_NAME</enum>]</member>
      <member><type>float</type> <name>m</name>[3][4]</member>
      <member><type>uint8_t</type> <name>tail</name></member>
    </type>
    <type category="union" name="VkU">
      <member><type>float</type> <name>f</name>[4]</member>
      <member><type>uint32_t</type> <name>u</name>[4]</member>
      <member><type>uint8_t</type> <name>b</name></member>
    </type>
    <type category="struct" name="VkHandles">
      <member><type>void</type>* <name>p</name></member>
      <member><type>VkInstance</type> <name>h</name></member>
      <member><type>VkBuffer</type> <name>nd</name></member>
      <member><type>PFN_vkFoo</type> <name>fn</name></member>
    </type>
    <type category="struct" name="VkEnums">
      <member><type>VkFormat</type> <name>f</name></member>
      <member><type>VkFooFlags</type> <name>fl</name></member>
      <member><type>VkFlags64</type> <name>big</name></member>
      <member><type>VkBool32</type> <name>b</name></member>
    </type>
    <type category="struct" name="VkBits">
      <member><type>uint32_t</type> <name>index</name>:24</member>
      <member><type>uint32_t</type> <name>mask</name>:8</member>
      <member><type>uint64_t</type> <name>reference</name></member>
    </type>
    <type category="struct" name="VkBitsSplit">
      <member><type>uint8_t</type> <name>a</name></member>
      <member><type>uint32_t</type> <name>x</name>:24</member>
      <member><type>uint32_t</type> <name>y</name>:8</member>
    </type>
    <type category="struct" name="VkPadAlias" alias="VkPad"/>
    <type category="struct" name="VkNeedsX"><member><type>Window</type> <name>w</name></member></type>
  </types>
  <enums name="API Constants" type="constants">
    <enum value="8" name="VK_MAX_NAME"/>
  </enums>
</registry>"#;

    fn layouts() -> Layouts {
        compute_layouts(SAMPLE).unwrap()
    }

    fn l(name: &str) -> Layout {
        layouts().ok.get(name).unwrap_or_else(|| panic!("brak {name}")).clone()
    }

    fn off(l: &Layout, f: &str) -> usize {
        l.offsets.iter().find(|(n, _)| n == f).unwrap_or_else(|| panic!("brak pola {f}")).1
    }

    #[test]
    fn padding_follows_alignment() {
        let p = l("VkPad");
        assert_eq!((p.size, p.align), (12, 4));
        assert_eq!((off(&p, "a"), off(&p, "b"), off(&p, "c")), (0, 4, 8));
    }

    #[test]
    fn nested_struct_and_double_alignment() {
        let o = l("VkOuter");
        assert_eq!((o.size, o.align), (16, 8));
        assert_eq!((off(&o, "i"), off(&o, "d")), (0, 8));
    }

    #[test]
    fn arrays_use_constants_and_multiple_dimensions() {
        let a = l("VkArrays");
        assert_eq!(off(&a, "name"), 0);
        assert_eq!(off(&a, "m"), 8);
        assert_eq!(off(&a, "tail"), 8 + 48);
        assert_eq!((a.size, a.align), (64, 4));
    }

    #[test]
    fn union_size_is_max_member_rounded_to_alignment() {
        let u = l("VkU");
        assert_eq!((u.size, u.align), (16, 4));
        assert!(u.offsets.iter().all(|(_, o)| *o == 0));
    }

    #[test]
    fn pointers_handles_and_function_pointers_are_eight_bytes() {
        let h = l("VkHandles");
        assert_eq!((h.size, h.align), (32, 8));
        assert_eq!((off(&h, "p"), off(&h, "h"), off(&h, "nd"), off(&h, "fn")), (0, 8, 16, 24));
    }

    #[test]
    fn enums_bitmasks_and_flags64() {
        let e = l("VkEnums");
        assert_eq!((off(&e, "f"), off(&e, "fl"), off(&e, "big"), off(&e, "b")), (0, 4, 8, 16));
        assert_eq!((e.size, e.align), (24, 8));
    }

    #[test]
    fn bitfields_are_packed_and_excluded_from_offsets() {
        let b = l("VkBits");
        assert_eq!((b.size, b.align), (16, 8));
        assert_eq!(b.offsets, vec![("reference".to_string(), 8)]);
        let s = l("VkBitsSplit");
        assert_eq!((s.size, s.align), (8, 4), "y:8 nie mieści się w jednostce po a + x:24");
        assert!(s.offsets.iter().all(|(n, _)| n == "a"));
    }

    #[test]
    fn alias_shares_layout_with_target() {
        assert_eq!(l("VkPadAlias"), l("VkPad"));
    }

    #[test]
    fn unresolved_non_pointer_type_is_reported_not_fatal() {
        let ls = layouts();
        assert!(ls.ok.contains_key("VkPad"));
        let why = ls.failed.get("VkNeedsX").expect("VkNeedsX powinna być w failed");
        assert!(why.contains("Window"), "{why}");
        assert!(!ls.ok.contains_key("VkNeedsX"));
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(compute_layouts("<registry><types>").is_err());
    }

    /// Wzorzec z prawdziwego kompilatora C (`tools/gen-layout-golden.py`): 1356 struktur.
    #[test]
    fn matches_real_c_compiler_for_every_struct_in_vulkan_core() {
        let xml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/vk.xml")).unwrap();
        let ls = compute_layouts(&xml).unwrap();
        let golden = include_str!("../tests/data/layout-golden.tsv");
        let mut sizes: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        let mut members: BTreeMap<&str, Vec<(&str, usize)>> = BTreeMap::new();
        for line in golden.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f[0] {
                "S" => { sizes.insert(f[1], (f[2].parse().unwrap(), f[3].parse().unwrap())); }
                "M" => members.entry(f[1]).or_default().push((f[2], f[3].parse().unwrap())),
                _ => {}
            }
        }
        assert!(sizes.len() >= 1350, "wzorzec za mały: {}", sizes.len());
        let mut errors: Vec<String> = Vec::new();
        for (name, (size, align)) in &sizes {
            match ls.ok.get(*name) {
                None => errors.push(format!("{name}: nie policzona ({:?})", ls.failed.get(*name))),
                Some(got) => {
                    if (got.size, got.align) != (*size, *align) {
                        errors.push(format!("{name}: rozmiar/wyrównanie {:?} != wzorzec {:?}", (got.size, got.align), (size, align)));
                    }
                    let want: Vec<(String, usize)> = members.get(*name).map(|v| v.iter().map(|(n, o)| (n.to_string(), *o)).collect()).unwrap_or_default();
                    if got.offsets != want {
                        errors.push(format!("{name}: przesunięcia pól różne od wzorca"));
                    }
                }
            }
        }
        assert!(errors.is_empty(), "{} błędów, pierwsze: {:#?}", errors.len(), &errors[..errors.len().min(12)]);
    }
}
