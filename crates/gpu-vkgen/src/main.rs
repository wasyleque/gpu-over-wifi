//! Generator stosu zdalnego GPU z rejestru Vulkan (`vk.xml`).
//!
//! Etap M0: analiza rejestru i klasyfikacja poleceń według tego, czy wymagają odpowiedzi.
//! Klasyfikacja steruje transportem: polecenia bez odpowiedzi są wysyłane partiami bez czekania.

mod layout;
mod meta;
mod structs;
mod stype;
mod table;

use std::collections::BTreeMap;

/// Jak polecenie zachowuje się w sieci.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// `void`, brak parametrów wyjściowych: wysyłane bez czekania.
    FireAndForget,
    /// Zwraca kod wyniku, brak parametrów wyjściowych: sukces zakładany, błąd zgłaszany później.
    DeferredResult,
    /// `vkCreate*` / `vkAllocate*`: uchwyt nadaje klient, bez czekania na serwer.
    OptimisticCreate,
    /// `vkGet*` / `vkEnumerate*` itp. z danymi wyjściowymi: cache lub zapytanie do serwera.
    Query,
    /// Oczekiwanie / synchronizacja: wymaga stanu serwera.
    Sync,
}

#[derive(Debug)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub is_ptr: bool,
    pub is_const: bool,
}

#[derive(Debug)]
pub struct Command {
    pub name: String,
    pub ret: String,
    pub params: Vec<Param>,
}

impl Command {
    pub fn out_params(&self) -> impl Iterator<Item = &Param> {
        self.params.iter().filter(|p| p.is_ptr && !p.is_const)
    }

    pub fn class(&self) -> Class {
        let has_out = self.out_params().next().is_some();
        let n = self.name.as_str();
        if n.starts_with("vkWait") || n.starts_with("vkAcquire") || n.starts_with("vkGetFenceStatus")
            || n.starts_with("vkGetSemaphoreCounterValue") || n.starts_with("vkDeviceWaitIdle")
            || n.starts_with("vkQueueWaitIdle")
        {
            return Class::Sync;
        }
        if has_out {
            if n.starts_with("vkCreate") || n.starts_with("vkAllocate") {
                Class::OptimisticCreate
            } else {
                Class::Query
            }
        } else if self.ret == "void" {
            Class::FireAndForget
        } else {
            Class::DeferredResult
        }
    }
}

/// Wczytuje polecenia (z rozwiązaniem aliasów) z treści `vk.xml`.
pub fn parse_commands(xml: &str) -> Result<Vec<Command>, String> {
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() },
    )
    .map_err(|e| e.to_string())?;
    let mut by_name: BTreeMap<String, Command> = BTreeMap::new();
    let mut aliases: Vec<(String, String)> = Vec::new();
    for c in doc.descendants().filter(|n| n.has_tag_name("command")) {
        if let (Some(a), Some(n)) = (c.attribute("alias"), c.attribute("name")) {
            aliases.push((n.to_string(), a.to_string()));
            continue;
        }
        let Some(proto) = c.children().find(|n| n.has_tag_name("proto")) else { continue };
        let name = child_text(&proto, "name");
        let ret = child_text(&proto, "type");
        let params = c
            .children()
            .filter(|n| n.has_tag_name("param"))
            .map(|p| {
                let text: String = p.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect();
                Param {
                    name: child_text(&p, "name"),
                    ty: child_text(&p, "type"),
                    is_ptr: text.contains('*'),
                    is_const: text.trim_start().starts_with("const"),
                }
            })
            .collect();
        by_name.insert(name.clone(), Command { name, ret, params });
    }
    // Alias ma tę samą sygnaturę co polecenie docelowe.
    for (alias, target) in aliases {
        if let Some(t) = by_name.get(&target) {
            let cmd = Command {
                name: alias.clone(),
                ret: t.ret.clone(),
                params: t.params.iter().map(|p| Param {
                    name: p.name.clone(), ty: p.ty.clone(), is_ptr: p.is_ptr, is_const: p.is_const,
                }).collect(),
            };
            by_name.insert(alias, cmd);
        }
    }
    Ok(by_name.into_values().collect())
}

fn child_text(node: &roxmltree::Node, tag: &str) -> String {
    node.children()
        .find(|n| n.has_tag_name(tag))
        .and_then(|n| n.text())
        .unwrap_or("")
        .to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--emit-meta") {
        let (Some(input), Some(output)) = (args.get(2), args.get(3)) else {
            eprintln!("Użycie: gpu-vkgen --emit-meta <vk.xml> <wyjście.rs>");
            std::process::exit(2);
        };
        let xml = std::fs::read_to_string(input).unwrap_or_else(|e| {
            eprintln!("Nie można odczytać {input}: {e}");
            std::process::exit(1);
        });
        match meta::compute_meta(&xml) {
            Ok(rows) => {
                if let Err(e) = std::fs::write(output, meta::meta_table_source(&rows)) {
                    eprintln!("Nie można zapisać {output}: {e}");
                    std::process::exit(1);
                }
                println!("zapisano {output}: {} struktur", rows.len());
            }
            Err(e) => {
                eprintln!("Błąd: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.get(1).map(String::as_str) == Some("--emit-stype-table") {
        let (Some(input), Some(output)) = (args.get(2), args.get(3)) else {
            eprintln!("Użycie: gpu-vkgen --emit-stype-table <vk.xml> <wyjście.rs>");
            std::process::exit(2);
        };
        let xml = std::fs::read_to_string(input).unwrap_or_else(|e| {
            eprintln!("Nie można odczytać {input}: {e}");
            std::process::exit(1);
        });
        match table::stype_table_source(&xml) {
            Ok(src) => {
                if let Err(e) = std::fs::write(output, &src) {
                    eprintln!("Nie można zapisać {output}: {e}");
                    std::process::exit(1);
                }
                println!("zapisano {output}: {} wierszy", src.matches("StypeEntry {").count().saturating_sub(1));
            }
            Err(e) => {
                eprintln!("Błąd: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let path = std::env::args().nth(1).unwrap_or_else(|| "third_party/vk.xml".into());
    let xml = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("Nie można odczytać {path}: {e}");
        std::process::exit(1);
    });
    let cmds = parse_commands(&xml).unwrap_or_else(|e| {
        eprintln!("Błąd parsowania: {e}");
        std::process::exit(1);
    });
    let mut counts: BTreeMap<Class, usize> = BTreeMap::new();
    for c in &cmds {
        *counts.entry(c.class()).or_default() += 1;
    }
    println!("Poleceń Vulkan (z aliasami): {}", cmds.len());
    for (class, n) in &counts {
        println!("  {class:?}: {n} ({:.0}%)", 100.0 * *n as f64 / cmds.len() as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<registry><commands>
      <command><proto><type>void</type> <name>vkCmdDraw</name></proto>
        <param><type>VkCommandBuffer</type> <name>commandBuffer</name></param>
        <param><type>uint32_t</type> <name>vertexCount</name></param></command>
      <command><proto><type>VkResult</type> <name>vkCreateBuffer</name></proto>
        <param><type>VkDevice</type> <name>device</name></param>
        <param>const <type>VkBufferCreateInfo</type>* <name>pCreateInfo</name></param>
        <param><type>VkBuffer</type>* <name>pBuffer</name></param></command>
      <command><proto><type>void</type> <name>vkGetPhysicalDeviceProperties</name></proto>
        <param><type>VkPhysicalDevice</type> <name>physicalDevice</name></param>
        <param><type>VkPhysicalDeviceProperties</type>* <name>pProperties</name></param></command>
      <command><proto><type>VkResult</type> <name>vkQueueSubmit</name></proto>
        <param><type>VkQueue</type> <name>queue</name></param>
        <param>const <type>VkSubmitInfo</type>* <name>pSubmits</name></param></command>
      <command><proto><type>VkResult</type> <name>vkWaitForFences</name></proto>
        <param><type>VkDevice</type> <name>device</name></param></command>
      <command name="vkGetPhysicalDeviceProperties2KHR" alias="vkGetPhysicalDeviceProperties"/>
    </commands></registry>"#;

    fn class_of(cmds: &[Command], name: &str) -> Class {
        cmds.iter().find(|c| c.name == name).unwrap().class()
    }

    #[test]
    fn classifies_by_network_behaviour() {
        let cmds = parse_commands(XML).unwrap();
        assert_eq!(class_of(&cmds, "vkCmdDraw"), Class::FireAndForget);
        assert_eq!(class_of(&cmds, "vkCreateBuffer"), Class::OptimisticCreate);
        assert_eq!(class_of(&cmds, "vkGetPhysicalDeviceProperties"), Class::Query);
        assert_eq!(class_of(&cmds, "vkQueueSubmit"), Class::DeferredResult);
        assert_eq!(class_of(&cmds, "vkWaitForFences"), Class::Sync);
    }

    #[test]
    fn aliases_inherit_target_signature() {
        let cmds = parse_commands(XML).unwrap();
        assert_eq!(class_of(&cmds, "vkGetPhysicalDeviceProperties2KHR"), Class::Query);
        assert_eq!(cmds.len(), 6);
    }

    #[test]
    fn const_pointers_are_inputs_not_outputs() {
        let cmds = parse_commands(XML).unwrap();
        let create = cmds.iter().find(|c| c.name == "vkCreateBuffer").unwrap();
        let outs: Vec<_> = create.out_params().map(|p| p.name.as_str()).collect();
        assert_eq!(outs, vec!["pBuffer"]);
    }
}
