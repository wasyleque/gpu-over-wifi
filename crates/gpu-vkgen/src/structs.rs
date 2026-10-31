//! Model struktur i unii Vulkan wczytany z `vk.xml`. Podstawa generatora koderów.
#![allow(dead_code)]

/// Jeden członek struktury lub unii.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub name: String,
    /// Typ bazowy bez `const` i `*`, np. `char`, `VkStructureType`.
    pub ty: String,
    /// Liczba gwiazdek wskaźnika: `const char* const*` => 2.
    pub ptr_depth: u8,
    /// Czy tekst przed typem zawiera `const`.
    pub is_const: bool,
    /// Wymiary tablicy stałej długości w kolejności z XML: `[3][4]` => ["3", "4"],
    /// `[VK_UUID_SIZE]` => ["VK_UUID_SIZE"]. Pusta, gdy to nie tablica.
    pub array_dims: Vec<String>,
    /// Surowa wartość atrybutu `len`, np. `enabledLayerCount,null-terminated`.
    pub len: Option<String>,
    /// Pierwszy element atrybutu `optional` równy `true` (`false,true` => false).
    pub optional: bool,
    /// Atrybut `values` (dla `sType`).
    pub values: Option<String>,
}

/// Struktura lub unia.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub members: Vec<Member>,
    /// Z atrybutu `structextends` (rozdzielone przecinkami).
    pub extends: Vec<String>,
    pub returned_only: bool,
    pub is_union: bool,
    /// Dla aliasów: nazwa struktury docelowej. Alias ma kopię członków celu.
    pub alias_of: Option<String>,
}

/// Wczytuje wszystkie struktury i unie (z rozwiązanymi aliasami) w kolejności z pliku.
pub fn parse_structs(xml: &str) -> Result<Vec<StructDef>, String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<registry><types>
      <type category="basetype">typedef <type>uint32_t</type> <name>VkFlags</name>;</type>
      <type category="struct" name="VkFoo" structextends="VkBar,VkBaz" returnedonly="true">
        <member values="VK_STRUCTURE_TYPE_FOO"><type>VkStructureType</type> <name>sType</name></member>
        <member optional="true">const <type>void</type>* <name>pNext</name></member>
        <member len="count,null-terminated">const <type>char</type>* const* <name>ppNames</name><comment>list of names</comment></member>
        <member optional="false,true"><type>uint32_t</type> <name>count</name></member>
        <member><type>float</type> <name>matrix</name>[3][4]</member>
        <member><type>char</type> <name>deviceName</name>[<enum>VK_MAX_NAME</enum>]</member>
      </type>
      <type category="struct" name="VkFooAlias" alias="VkFoo"/>
      <type category="union" name="VkU"><member><type>float</type> <name>f</name>[4]</member></type>
    </types></registry>"#;

    fn find<'a>(all: &'a [StructDef], name: &str) -> &'a StructDef {
        all.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("brak {name}"))
    }

    fn member<'a>(s: &'a StructDef, name: &str) -> &'a Member {
        s.members.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("brak {name}"))
    }

    #[test]
    fn sample_keeps_file_order_and_ignores_other_types() {
        let all = parse_structs(SAMPLE).unwrap();
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["VkFoo", "VkFooAlias", "VkU"]);
    }

    #[test]
    fn sample_struct_attributes() {
        let all = parse_structs(SAMPLE).unwrap();
        let foo = find(&all, "VkFoo");
        assert_eq!(foo.extends, vec!["VkBar", "VkBaz"]);
        assert!(foo.returned_only);
        assert!(!foo.is_union);
        assert_eq!(foo.alias_of, None);
        let names: Vec<&str> = foo.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["sType", "pNext", "ppNames", "count", "matrix", "deviceName"]);
    }

    #[test]
    fn sample_member_details() {
        let all = parse_structs(SAMPLE).unwrap();
        let foo = find(&all, "VkFoo");
        let stype = member(foo, "sType");
        assert_eq!(stype.ty, "VkStructureType");
        assert_eq!(stype.ptr_depth, 0);
        assert_eq!(stype.values.as_deref(), Some("VK_STRUCTURE_TYPE_FOO"));

        let pnext = member(foo, "pNext");
        assert_eq!((pnext.ty.as_str(), pnext.ptr_depth, pnext.is_const, pnext.optional), ("void", 1, true, true));

        // Komentarz wewnątrz <member> nie może trafić do nazwy ani typu.
        let names = member(foo, "ppNames");
        assert_eq!((names.ty.as_str(), names.ptr_depth, names.is_const), ("char", 2, true));
        assert_eq!(names.len.as_deref(), Some("count,null-terminated"));

        assert!(!member(foo, "count").optional, "false,true => pierwszy element to false");

        assert_eq!(member(foo, "matrix").array_dims, vec!["3", "4"]);
        assert_eq!(member(foo, "deviceName").array_dims, vec!["VK_MAX_NAME"]);
        assert!(member(foo, "count").array_dims.is_empty());
    }

    #[test]
    fn sample_alias_copies_members_and_union_is_marked() {
        let all = parse_structs(SAMPLE).unwrap();
        let alias = find(&all, "VkFooAlias");
        let foo = find(&all, "VkFoo");
        assert_eq!(alias.alias_of.as_deref(), Some("VkFoo"));
        assert_eq!(alias.members, foo.members);
        let u = find(&all, "VkU");
        assert!(u.is_union);
        assert_eq!(member(u, "f").array_dims, vec!["4"]);
    }

    #[test]
    fn alias_may_precede_its_target_in_the_file() {
        let xml = r#"<registry><types>
          <type category="struct" name="VkA2" alias="VkA"/>
          <type category="struct" name="VkA"><member><type>uint32_t</type> <name>x</name></member></type>
        </types></registry>"#;
        let all = parse_structs(xml).unwrap();
        assert_eq!(find(&all, "VkA2").members.len(), 1);
    }

    #[test]
    fn malformed_xml_is_an_error_not_a_panic() {
        assert!(parse_structs("<registry><types>").is_err());
    }

    fn real() -> Vec<StructDef> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/vk.xml");
        parse_structs(&std::fs::read_to_string(path).expect("third_party/vk.xml")).unwrap()
    }

    #[test]
    fn real_registry_application_info() {
        let all = real();
        let s = find(&all, "VkApplicationInfo");
        let names: Vec<&str> = s.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["sType", "pNext", "pApplicationName", "applicationVersion", "pEngineName", "engineVersion", "apiVersion"]
        );
        assert_eq!(member(s, "sType").values.as_deref(), Some("VK_STRUCTURE_TYPE_APPLICATION_INFO"));
        let n = member(s, "pApplicationName");
        assert_eq!((n.ty.as_str(), n.ptr_depth, n.is_const, n.optional), ("char", 1, true, true));
        assert_eq!(n.len.as_deref(), Some("null-terminated"));
    }

    #[test]
    fn real_registry_instance_create_info_string_array() {
        let all = real();
        let m = member(find(&all, "VkInstanceCreateInfo"), "ppEnabledLayerNames");
        assert_eq!((m.ty.as_str(), m.ptr_depth, m.is_const), ("char", 2, true));
        assert_eq!(m.len.as_deref(), Some("enabledLayerCount,null-terminated"));
        // Komentarz w XML ("Ordered list of layer names...") nie może przeciec do nazwy.
        assert_eq!(m.name, "ppEnabledLayerNames");
    }

    #[test]
    fn real_registry_fixed_arrays_and_returned_only() {
        let all = real();
        let p = find(&all, "VkPhysicalDeviceProperties");
        assert!(p.returned_only);
        let name = member(p, "deviceName");
        assert_eq!(name.ty, "char");
        assert_eq!(name.array_dims, vec!["VK_MAX_PHYSICAL_DEVICE_NAME_SIZE"]);
        assert_eq!(member(p, "pipelineCacheUUID").array_dims, vec!["VK_UUID_SIZE"]);
        assert_eq!(member(find(&all, "VkTransformMatrixKHR"), "matrix").array_dims, vec!["3", "4"]);
    }

    #[test]
    fn real_registry_extends_union_alias_and_counts() {
        let all = real();
        assert!(find(&all, "VkDebugUtilsMessengerCreateInfoEXT").extends.contains(&"VkInstanceCreateInfo".to_string()));
        assert!(find(&all, "VkClearColorValue").is_union);
        let a = find(&all, "VkBufferUsageFlags2CreateInfoKHR");
        assert_eq!(a.alias_of.as_deref(), Some("VkBufferUsageFlags2CreateInfo"));
        let names: Vec<&str> = a.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["sType", "pNext", "usage"]);
        assert!(all.len() >= 1700, "oczekiwano >= 1700 struktur i unii, jest {}", all.len());
        assert!(all.iter().filter(|s| s.alias_of.is_some()).count() >= 250);
        assert!(all.iter().all(|s| !s.name.is_empty() && s.members.iter().all(|m| !m.name.is_empty() && !m.ty.is_empty())));
    }
}
