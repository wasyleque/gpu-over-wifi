#![allow(dead_code)]

use std::collections::BTreeMap;

use roxmltree::Document;

use crate::structs::{parse_structs, Member, StructDef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub size: usize,
    pub align: usize,
    pub offsets: Vec<(String, usize)>,
}

#[derive(Debug, Default)]
pub struct Layouts {
    pub ok: BTreeMap<String, Layout>,
    pub failed: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
struct TypeInfo {
    size: usize,
    align: usize,
}

fn primitive(name: &str) -> Option<TypeInfo> {
    let t = match name {
        "char" | "uint8_t" | "int8_t" | "signed char" | "unsigned char" => TypeInfo { size: 1, align: 1 },
        "uint16_t" | "int16_t" | "short" | "unsigned short" => TypeInfo { size: 2, align: 2 },
        "uint32_t" | "int32_t" | "int" | "unsigned int" | "float" | "VkBool32" => {
            TypeInfo { size: 4, align: 4 }
        }
        "uint64_t" | "int64_t" | "double" | "size_t" | "long" | "unsigned long" | "long long"
        | "unsigned long long" => TypeInfo { size: 8, align: 8 },
        _ => return None,
    };
    Some(t)
}

fn round_up(v: usize, a: usize) -> usize {
    if a <= 1 {
        v
    } else {
        v.div_ceil(a) * a
    }
}

struct TypeTable {
    map: BTreeMap<String, TypeInfo>,
    constants: BTreeMap<String, u64>,
}

fn parse_constants(doc: &Document) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for node in doc.descendants() {
        if !node.is_element() || !node.has_tag_name("enums") {
            continue;
        }
        let is_constants = node.attribute("type") == Some("constants");
        if !is_constants {
            continue;
        }
        for child in node.children() {
            if !child.is_element() || !child.has_tag_name("enum") {
                continue;
            }
            let name = match child.attribute("name") {
                Some(n) => n,
                None => continue,
            };
            let value = match child.attribute("value") {
                Some(v) => v,
                None => continue,
            };
            if let Ok(n) = value.parse::<u64>() {
                out.insert(name.to_string(), n);
            }
        }
    }
    out
}

/// Nazwa typu z elementu potomnego `<name>` (tak zapisane są basetype, bitmask i handle).
fn child_name(node: &roxmltree::Node) -> Option<String> {
    node.children()
        .find(|c| c.is_element() && c.has_tag_name("name"))
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
}

fn parse_types(doc: &Document) -> TypeTable {
    let mut map: BTreeMap<String, TypeInfo> = BTreeMap::new();
    let mut enum64: BTreeMap<String, bool> = BTreeMap::new();

    for node in doc.descendants() {
        if !node.is_element() || !node.has_tag_name("enums") {
            continue;
        }
        if let Some(name) = node.attribute("name") {
            if node.attribute("bitwidth") == Some("64") {
                enum64.insert(name.to_string(), true);
            }
        }
    }

    let mut aliases: Vec<(String, String)> = Vec::new();
    for node in doc.descendants() {
        if !node.is_element() || !node.has_tag_name("type") {
            continue;
        }
        let category = node.attribute("category");
        let name_attr = node.attribute("name");
        // Alias typu (np. VkPipelineStageFlags2KHR -> VkPipelineStageFlags2): rozwiązywany po pętli.
        if let (Some(name), Some(target)) = (name_attr, node.attribute("alias")) {
            if category != Some("struct") && category != Some("union") {
                aliases.push((name.to_string(), target.to_string()));
            }
            continue;
        }

        match category {
            None => {
                if let Some(name) = name_attr {
                    if let Some(t) = primitive(name) {
                        map.insert(name.to_string(), t);
                    } else if node.attribute("requires").is_some_and(|r| r.starts_with("vk_video/")) {
                        // Typy kodeków wideo z zewnętrznych nagłówków używane przez wartość to wyliczenia.
                        map.insert(name.to_string(), TypeInfo { size: 4, align: 4 });
                    }
                }
            }
            Some("basetype") => {
                let name_owned = match child_name(&node).or_else(|| name_attr.map(String::from)) {
                    Some(n) => n,
                    None => continue,
                };
                let name = name_owned.as_str();
                let mut has_star = false;
                let mut inner_type: Option<String> = None;
                for child in node.children() {
                    if child.is_text() {
                        if let Some(t) = child.text() {
                            if t.contains('*') {
                                has_star = true;
                            }
                        }
                    } else if child.is_element() && child.has_tag_name("type") && inner_type.is_none() {
                        inner_type = child.text().map(|s| s.trim().to_string());
                    }
                }
                if has_star {
                    map.insert(name.to_string(), TypeInfo { size: 8, align: 8 });
                } else if let Some(it) = inner_type {
                    if let Some(t) = map.get(&it).copied().or_else(|| primitive(&it)) {
                        map.insert(name.to_string(), t);
                    }
                }
            }
            Some("bitmask") => {
                let name_owned = match child_name(&node).or_else(|| name_attr.map(String::from)) {
                    Some(n) => n,
                    None => continue,
                };
                let name = name_owned.as_str();
                let mut inner_type: Option<String> = None;
                for child in node.children() {
                    if child.is_element() && child.has_tag_name("type") && inner_type.is_none() {
                        inner_type = child.text().map(|s| s.trim().to_string());
                    }
                }
                if let Some(it) = inner_type {
                    if let Some(t) = map.get(&it).copied().or_else(|| primitive(&it)) {
                        map.insert(name.to_string(), t);
                    }
                }
            }
            Some("handle") => {
                if let Some(name) = child_name(&node).or_else(|| name_attr.map(String::from)) {
                    map.insert(name, TypeInfo { size: 8, align: 8 });
                }
            }
            Some("funcpointer") => {
                let mut name: Option<String> = None;
                for child in node.descendants() {
                    if child.is_element() && child.has_tag_name("name") {
                        name = child.text().map(|s| s.trim().to_string());
                        break;
                    }
                }
                if let Some(n) = name {
                    map.insert(n, TypeInfo { size: 8, align: 8 });
                }
            }
            Some("enum") => {
                if let Some(name) = name_attr {
                    let t = if enum64.get(name).copied().unwrap_or(false) {
                        TypeInfo { size: 8, align: 8 }
                    } else {
                        TypeInfo { size: 4, align: 4 }
                    };
                    map.insert(name.to_string(), t);
                }
            }
            _ => {}
        }
    }

    // Aliasy mogą wskazywać na inne aliasy: powtarzamy, aż nic się nie zmienia.
    loop {
        let before = map.len();
        for (name, target) in &aliases {
            if !map.contains_key(name) {
                if let Some(t) = map.get(target).copied() {
                    map.insert(name.clone(), t);
                }
            }
        }
        if map.len() == before {
            break;
        }
    }

    TypeTable { map, constants: parse_constants(doc) }
}

fn resolve_type<'a>(
    ty: &str,
    ptr_depth: u8,
    table: &'a TypeTable,
    structs: &'a BTreeMap<String, &'a StructDef>,
    layouts: &'a BTreeMap<String, Layout>,
) -> Result<TypeInfo, String> {
    if ptr_depth > 0 {
        return Ok(TypeInfo { size: 8, align: 8 });
    }
    if let Some(t) = primitive(ty) {
        return Ok(t);
    }
    if let Some(t) = table.map.get(ty) {
        return Ok(*t);
    }
    if let Some(l) = layouts.get(ty) {
        return Ok(TypeInfo { size: l.size, align: l.align });
    }
    if structs.contains_key(ty) {
        return Err(format!("unresolved type: {}", ty));
    }
    Err(format!("unknown type: {}", ty))
}

fn eval_dim(dim: &str, constants: &BTreeMap<String, u64>) -> Result<usize, String> {
    let d = dim.trim();
    if let Ok(n) = d.parse::<usize>() {
        return Ok(n);
    }
    if let Some(n) = constants.get(d) {
        return Ok(*n as usize);
    }
    Err(format!("unknown array dimension: {}", d))
}

fn member_type_info(
    m: &Member,
    table: &TypeTable,
    structs: &BTreeMap<String, &StructDef>,
    layouts: &BTreeMap<String, Layout>,
) -> Result<TypeInfo, String> {
    let base = resolve_type(&m.ty, m.ptr_depth, table, structs, layouts)?;
    if m.array_dims.is_empty() {
        return Ok(base);
    }
    let mut count: usize = 1;
    for d in &m.array_dims {
        let n = eval_dim(d, &table.constants)?;
        count = count.saturating_mul(n);
    }
    Ok(TypeInfo {
        size: base.size.saturating_mul(count),
        align: base.align,
    })
}

fn compute_struct(
    def: &StructDef,
    table: &TypeTable,
    structs: &BTreeMap<String, &StructDef>,
    layouts: &BTreeMap<String, Layout>,
) -> Result<Layout, String> {
    let mut offsets: Vec<(String, usize)> = Vec::new();
    let mut struct_align: usize = 1;

    if def.is_union {
        let mut max_size: usize = 0;
        for m in &def.members {
            if m.bit_width.is_some() {
                return Err(format!("bitfield in union: {}", m.name));
            }
            let ti = member_type_info(m, table, structs, layouts)?;
            struct_align = struct_align.max(ti.align);
            max_size = max_size.max(ti.size);
            offsets.push((m.name.clone(), 0));
        }
        let size = round_up(max_size, struct_align);
        return Ok(Layout { size, align: struct_align, offsets });
    }

    let mut end: usize = 0;
    let mut bit_pos: usize = 0;
    let mut in_bits = false;

    for m in &def.members {
        if let Some(w) = m.bit_width {
            let ti = resolve_type(&m.ty, m.ptr_depth, table, structs, layouts)?;
            struct_align = struct_align.max(ti.align);
            let unit_bits = ti.size.saturating_mul(8);
            if unit_bits == 0 {
                return Err(format!("zero-size bitfield type: {}", m.ty));
            }
            if !in_bits {
                bit_pos = end.saturating_mul(8);
                in_bits = true;
            }
            let w_usize = w as usize;
            if (bit_pos % unit_bits) + w_usize > unit_bits {
                bit_pos = round_up(bit_pos, unit_bits);
            }
            bit_pos = bit_pos.saturating_add(w_usize);
            end = bit_pos.div_ceil(8);
        } else {
            if in_bits {
                end = bit_pos.div_ceil(8);
                in_bits = false;
            }
            let ti = member_type_info(m, table, structs, layouts)?;
            struct_align = struct_align.max(ti.align);
            let off = round_up(end, ti.align);
            offsets.push((m.name.clone(), off));
            end = off.saturating_add(ti.size);
        }
    }
    if in_bits {
        end = bit_pos.div_ceil(8);
    }

    let size = round_up(end, struct_align);
    Ok(Layout { size, align: struct_align, offsets })
}

pub fn compute_layouts(xml: &str) -> Result<Layouts, String> {
    let doc = Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() },
    )
    .map_err(|e| e.to_string())?;

    let table = parse_types(&doc);
    let defs = parse_structs(xml)?;

    let mut by_name: BTreeMap<String, &StructDef> = BTreeMap::new();
    for d in &defs {
        by_name.insert(d.name.clone(), d);
    }

    let mut layouts = Layouts::default();
    let mut computed: BTreeMap<String, Layout> = BTreeMap::new();

    let mut pending: Vec<&StructDef> = defs.iter().collect();
    let mut progress = true;
    while progress && !pending.is_empty() {
        progress = false;
        let mut next: Vec<&StructDef> = Vec::new();
        for def in pending {
            if let Some(target) = &def.alias_of {
                if let Some(l) = computed.get(target) {
                    computed.insert(def.name.clone(), l.clone());
                    progress = true;
                } else if by_name.contains_key(target) {
                    next.push(def);
                } else {
                    layouts
                        .failed
                        .insert(def.name.clone(), format!("alias target missing: {}", target));
                    progress = true;
                }
                continue;
            }
            match compute_struct(def, &table, &by_name, &computed) {
                Ok(l) => {
                    computed.insert(def.name.clone(), l);
                    progress = true;
                }
                Err(e) => {
                    let mut dep_missing = false;
                    for m in &def.members {
                        if m.ptr_depth == 0
                            && !primitive(&m.ty).is_some()
                            && !table.map.contains_key(&m.ty)
                            && !computed.contains_key(&m.ty)
                            && by_name.contains_key(&m.ty)
                        {
                            dep_missing = true;
                            break;
                        }
                    }
                    if dep_missing {
                        next.push(def);
                    } else {
                        layouts.failed.insert(def.name.clone(), e);
                        progress = true;
                    }
                }
            }
        }
        pending = next;
    }

    for def in pending {
        if !layouts.failed.contains_key(&def.name) {
            let reason = if let Some(target) = &def.alias_of {
                format!("alias target unresolved: {}", target)
            } else {
                "unresolved dependencies".to_string()
            };
            layouts.failed.insert(def.name.clone(), reason);
        }
    }

    layouts.ok = computed;
    Ok(layouts)
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
        assert_eq!((a.size, a.align), (60, 4), "8 + 48 + 1 = 57, zaokrąglone do wyrównania 4");
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
