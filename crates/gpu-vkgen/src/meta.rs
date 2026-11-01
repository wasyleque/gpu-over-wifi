//! Generator metadanych członków struktur (`gpu-proto/src/meta_table.rs`).
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};

use crate::layout::compute_layouts;
use crate::structs::{parse_structs, Member};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cat {
    Handle,
    Enum,
    Bitmask,
    Basetype,
    Struct,
    Union,
    FuncPointer,
}

struct Types {
    cat: HashMap<String, Cat>,
    alias: HashMap<String, String>,
    /// Rozmiary typów prostych: prymitywy C, basetype, bitmask, enum.
    scalar: HashMap<String, u32>,
    constants: HashMap<String, u32>,
}

fn prim_size(t: &str) -> Option<u32> {
    Some(match t {
        "char" | "int8_t" | "uint8_t" => 1,
        "int16_t" | "uint16_t" => 2,
        "int" | "int32_t" | "uint32_t" | "float" => 4,
        "int64_t" | "uint64_t" | "double" | "size_t" => 8,
        _ => return None,
    })
}

fn load_types(xml: &str) -> Result<Types, String> {
    let doc = roxmltree::Document::parse_with_options(xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).map_err(|e| e.to_string())?;
    let mut t = Types { cat: HashMap::new(), alias: HashMap::new(), scalar: HashMap::new(), constants: HashMap::new() };
    let mut wide_enums = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        match n.tag_name().name() {
            "enums" => {
                if n.attribute("bitwidth") == Some("64") {
                    if let Some(name) = n.attribute("name") {
                        wide_enums.push(name.to_string());
                    }
                }
                if n.attribute("name") == Some("API Constants") {
                    for e in n.children().filter(|c| c.is_element() && c.has_tag_name("enum")) {
                        if let (Some(name), Some(v)) = (e.attribute("name"), e.attribute("value")) {
                            if let Ok(v) = v.trim().parse::<u32>() {
                                t.constants.insert(name.to_string(), v);
                            }
                        }
                    }
                }
            }
            "type" => {
                let Some(category) = n.attribute("category") else { continue };
                let name = n.attribute("name").map(str::to_string).or_else(|| n.descendants().find(|c| c.has_tag_name("name")).and_then(|c| c.text()).map(str::to_string));
                let Some(name) = name else { continue };
                if let Some(a) = n.attribute("alias") {
                    t.alias.insert(name, a.to_string());
                    continue;
                }
                let inner = n.children().find(|c| c.has_tag_name("type")).and_then(|c| c.text()).unwrap_or("").to_string();
                let cat = match category {
                    "handle" => Cat::Handle,
                    "enum" => Cat::Enum,
                    "bitmask" => Cat::Bitmask,
                    "basetype" => Cat::Basetype,
                    "struct" => Cat::Struct,
                    "union" => Cat::Union,
                    "funcpointer" => Cat::FuncPointer,
                    _ => continue,
                };
                if matches!(cat, Cat::Bitmask | Cat::Basetype) {
                    let size = prim_size(&inner).or(match inner.as_str() {
                        "VkFlags" => Some(4),
                        "VkFlags64" => Some(8),
                        _ => None,
                    });
                    if let Some(s) = size {
                        t.scalar.insert(name.clone(), s);
                    }
                }
                t.cat.insert(name, cat);
            }
            _ => {}
        }
    }
    for e in t.cat.iter().filter(|(_, c)| **c == Cat::Enum).map(|(n, _)| n.clone()).collect::<Vec<_>>() {
        t.scalar.insert(e.clone(), if wide_enums.contains(&e) { 8 } else { 4 });
    }
    Ok(t)
}

impl Types {
    fn resolve<'a>(&'a self, name: &'a str) -> &'a str {
        let mut n = name;
        for _ in 0..8 {
            match self.alias.get(n) {
                Some(a) => n = a,
                None => break,
            }
        }
        n
    }

    fn dim(&self, d: &str) -> Option<u32> {
        d.trim().parse().ok().or_else(|| self.constants.get(d.trim()).copied())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRow {
    pub name: String,
    pub offset: u32,
    pub shape: &'static str,
    pub elem: &'static str,
    pub type_name: String,
    pub elem_size: u32,
    pub count: u32,
    pub ptr_depth: u8,
    pub is_const: bool,
    pub optional: bool,
    pub len: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructRow {
    pub name: String,
    pub size: u32,
    pub align: u32,
    pub is_union: bool,
    pub members: Vec<MemberRow>,
}

/// Wylicza wiersze metadanych dla wszystkich struktur o poprawnie policzonym układzie, posortowane po nazwie.
pub fn compute_meta(xml: &str) -> Result<Vec<StructRow>, String> {
    let types = load_types(xml)?;
    let structs = parse_structs(xml)?;
    let layouts = compute_layouts(xml)?;
    let mut out: BTreeMap<String, StructRow> = BTreeMap::new();
    for s in structs.iter().filter(|s| s.alias_of.is_none()) {
        let Some(layout) = layouts.ok.get(&s.name) else { continue };
        let offsets: HashMap<&str, usize> = layout.offsets.iter().map(|(n, o)| (n.as_str(), *o)).collect();
        let mut members = Vec::new();
        for m in &s.members {
            let off = offsets.get(m.name.as_str()).copied().unwrap_or(0) as u32;
            members.push(classify(&types, &layouts.ok, m, off));
        }
        out.insert(s.name.clone(), StructRow { name: s.name.clone(), size: layout.size as u32, align: layout.align as u32, is_union: s.is_union, members });
    }
    Ok(out.into_values().collect())
}

fn classify(t: &Types, layouts: &BTreeMap<String, crate::layout::Layout>, m: &Member, offset: u32) -> MemberRow {
    let ty = t.resolve(&m.ty).to_string();
    let mut row = MemberRow {
        name: m.name.clone(),
        offset,
        shape: "Value",
        elem: "External",
        type_name: ty.clone(),
        elem_size: 0,
        count: 1,
        ptr_depth: m.ptr_depth,
        is_const: m.is_const,
        optional: m.optional,
        len: m.len.clone().unwrap_or_default(),
    };
    if m.bit_width.is_some() {
        row.shape = "Bitfield";
        row.elem = "Scalar";
        return row;
    }
    // Rodzaj elementu.
    let (elem, size) = if ty == "void" {
        ("Void", 0)
    } else if ty == "char" {
        ("Char", 1)
    } else if let Some(s) = prim_size(&ty).or_else(|| t.scalar.get(&ty).copied()) {
        ("Scalar", s)
    } else {
        match t.cat.get(&ty) {
            Some(Cat::Handle) => ("Handle", 8),
            Some(Cat::FuncPointer) => ("FuncPtr", 8),
            Some(Cat::Struct) => ("Struct", layouts.get(&ty).map(|l| l.size as u32).unwrap_or(0)),
            Some(Cat::Union) => ("Union", layouts.get(&ty).map(|l| l.size as u32).unwrap_or(0)),
            _ => ("External", 0),
        }
    };
    row.elem = elem;
    row.elem_size = size;
    if m.name == "pNext" && m.ptr_depth == 1 && ty == "void" {
        row.shape = "PNext";
    } else if m.ptr_depth > 0 {
        row.shape = "Pointer";
    } else if !m.array_dims.is_empty() {
        row.count = m.array_dims.iter().map(|d| t.dim(d).unwrap_or(0)).product();
    }
    row
}

/// Źródło `meta_table.rs`.
pub fn meta_table_source(rows: &[StructRow]) -> String {
    let mut members = String::new();
    let mut structs = String::new();
    let mut idx = 0usize;
    for r in rows {
        structs.push_str(&format!(
            "    StructMeta {{ name: {:?}, size: {}, align: {}, is_union: {}, first: {}, count: {} }},\n",
            r.name, r.size, r.align, r.is_union, idx, r.members.len()
        ));
        for m in &r.members {
            members.push_str(&format!(
                "    MemberMeta {{ name: {:?}, offset: {}, shape: Shape::{}, elem: Elem::{}, type_name: {:?}, elem_size: {}, count: {}, ptr_depth: {}, is_const: {}, optional: {}, len: {:?} }},\n",
                m.name, m.offset, m.shape, m.elem, m.type_name, m.elem_size, m.count, m.ptr_depth, m.is_const, m.optional, m.len
            ));
        }
        idx += r.members.len();
    }
    format!(
        "//! GENEROWANY przez `gpu-vkgen --emit-meta`. Nie edytować ręcznie.\n\nuse crate::meta::{{Elem, MemberMeta, Shape, StructMeta}};\n\n/// Posortowana po nazwie.\npub const STRUCTS: &[StructMeta] = &[\n{structs}];\n\npub const MEMBERS: &[MemberMeta] = &[\n{members}];\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real() -> Vec<StructRow> {
        let xml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/vk.xml")).unwrap();
        compute_meta(&xml).unwrap()
    }

    fn find<'a>(rows: &'a [StructRow], n: &str) -> &'a StructRow {
        rows.iter().find(|r| r.name == n).unwrap_or_else(|| panic!("brak {n}"))
    }

    fn mem<'a>(r: &'a StructRow, n: &str) -> &'a MemberRow {
        r.members.iter().find(|m| m.name == n).unwrap_or_else(|| panic!("brak {}.{n}", r.name))
    }

    #[test]
    fn instance_create_info_members_are_classified() {
        let rows = real();
        let r = find(&rows, "VkInstanceCreateInfo");
        assert_eq!((r.size, r.align), (64, 8));
        assert_eq!((mem(r, "sType").shape, mem(r, "sType").elem, mem(r, "sType").elem_size), ("Value", "Scalar", 4));
        assert_eq!(mem(r, "pNext").shape, "PNext");
        let app = mem(r, "pApplicationInfo");
        assert_eq!((app.shape, app.elem, app.type_name.as_str(), app.elem_size, app.is_const, app.offset), ("Pointer", "Struct", "VkApplicationInfo", 48, true, 24));
        let names = mem(r, "ppEnabledLayerNames");
        assert_eq!((names.shape, names.elem, names.ptr_depth, names.len.as_str(), names.offset), ("Pointer", "Char", 2, "enabledLayerCount,null-terminated", 40));
        assert_eq!(mem(r, "enabledExtensionCount").offset, 48);
    }

    #[test]
    fn arrays_inline_structs_handles_and_funcptrs() {
        let rows = real();
        let p = find(&rows, "VkPhysicalDeviceProperties");
        let name = mem(p, "deviceName");
        assert_eq!((name.shape, name.elem, name.count), ("Value", "Char", 256));
        assert_eq!(mem(p, "pipelineCacheUUID").count, 16);
        assert_eq!((mem(p, "limits").elem, mem(p, "limits").elem_size), ("Struct", 504));
        let p2 = find(&rows, "VkPhysicalDeviceProperties2");
        assert_eq!((mem(p2, "properties").elem, mem(p2, "properties").type_name.as_str()), ("Struct", "VkPhysicalDeviceProperties"));
        let m = find(&rows, "VkTransformMatrixKHR");
        assert_eq!(mem(m, "matrix").count, 12, "wymiary 3x4 mnożymy");
        let bind = find(&rows, "VkBindBufferMemoryInfo");
        assert_eq!((mem(bind, "buffer").elem, mem(bind, "buffer").elem_size), ("Handle", 8));
        assert_eq!(mem(bind, "memoryOffset").elem_size, 8, "VkDeviceSize to 8 B");
        let alloc = find(&rows, "VkAllocationCallbacks");
        assert_eq!(mem(alloc, "pfnAllocation").elem, "FuncPtr");
        assert_eq!(mem(alloc, "pUserData").elem, "Void");
    }

    #[test]
    fn lengths_enums_flags_and_bools() {
        let rows = real();
        let sm = find(&rows, "VkShaderModuleCreateInfo");
        assert!(mem(sm, "pCode").len.contains("codeSize"));
        let b = find(&rows, "VkBufferCreateInfo");
        assert_eq!((mem(b, "usage").elem, mem(b, "usage").elem_size), ("Scalar", 4), "bitmask VkBufferUsageFlags");
        assert_eq!((mem(b, "sharingMode").elem_size, mem(b, "pQueueFamilyIndices").len.as_str()), (4, "queueFamilyIndexCount"));
        let f = find(&rows, "VkPhysicalDeviceFeatures");
        assert_eq!((mem(f, "robustBufferAccess").elem_size, f.size), (4, 220));
    }

    #[test]
    fn every_member_is_sane_for_every_struct() {
        let rows = real();
        assert!(rows.len() > 1200, "{}", rows.len());
        let mut bad = Vec::new();
        for r in &rows {
            let mut last = 0u32;
            for m in &r.members {
                if m.offset + (m.elem_size * m.count).min(r.size) > r.size && m.shape == "Value" && !r.is_union {
                    bad.push(format!("{}.{} wychodzi poza strukturę", r.name, m.name));
                }
                if !r.is_union && m.shape != "Bitfield" && m.offset < last {
                    bad.push(format!("{}.{} offset maleje", r.name, m.name));
                }
                if m.shape == "Value" && m.elem != "External" && m.count == 0 {
                    bad.push(format!("{}.{} count 0", r.name, m.name));
                }
                last = m.offset;
            }
        }
        assert!(bad.is_empty(), "{} problemów, pierwsze: {:#?}", bad.len(), &bad[..bad.len().min(8)]);
    }

    #[test]
    fn source_is_sorted_and_indexes_are_consistent() {
        let rows = real();
        assert!(rows.windows(2).all(|w| w[0].name < w[1].name));
        let src = meta_table_source(&rows);
        assert!(src.contains("StructMeta { name: \"VkInstanceCreateInfo\", size: 64, align: 8, is_union: false, first: "));
    }
}
