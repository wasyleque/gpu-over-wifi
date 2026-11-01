//! Rejestr uchwytów nadanych przez klienta: identyfikator (u64 z protokołu) -> prawdziwy uchwyt serwera.
//! Wpis pamięta rodzaj obiektu (typ Vulkana), obiekt nadrzędny (urządzenie) i rozmiar (dla pamięci).

use std::collections::HashMap;

use gpu_vkcodec::codec::Ctx;
use gpu_vkcodec::CodecError;

pub const MAX_HANDLES: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub kind: &'static str,
    pub real: u64,
    /// Identyfikator obiektu nadrzędnego (pierwszy parametr-uchwyt polecenia tworzącego), 0 gdy brak.
    pub parent: u64,
    pub size: u64,
}

#[derive(Default)]
pub struct Registry {
    map: HashMap<u64, Entry>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rejestruje identyfikator. Błąd, gdy 0, już zajęty albo limit osiągnięty (klient nie może nadpisać cudzego wpisu).
    pub fn register(&mut self, id: u64, entry: Entry) -> Result<(), CodecError> {
        if id == 0 || self.map.contains_key(&id) || self.map.len() >= MAX_HANDLES {
            return Err(CodecError::Handle(id));
        }
        self.map.insert(id, entry);
        Ok(())
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.map.get(&id)
    }

    pub fn unregister(&mut self, id: u64) -> Option<Entry> {
        self.map.remove(&id)
    }

    /// Wszystkie wpisy danego rodzaju (identyfikator, wpis).
    pub fn entries_of_kind(&self, kind: &str) -> Vec<(u64, Entry)> {
        self.map.iter().filter(|(_, e)| e.kind == kind).map(|(i, e)| (*i, *e)).collect()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Ctx for Registry {
    /// Serwer nie odsyła uchwytów w odpowiedziach M2a (klient nadaje identyfikatory sam).
    fn handle_to_wire(&self, _ty: &str, raw: u64) -> Result<u64, CodecError> {
        Err(CodecError::Handle(raw))
    }

    /// Identyfikator z sieci -> prawdziwy uchwyt; typ musi się zgadzać z zarejestrowanym.
    /// Wartość 0 (VK_NULL_HANDLE) przechodzi jako 0.
    fn wire_to_handle(&self, ty: &str, wire: u64) -> Result<u64, CodecError> {
        if wire == 0 {
            return Ok(0);
        }
        match self.map.get(&wire) {
            Some(e) if e.kind == ty => Ok(e.real),
            _ => Err(CodecError::Handle(wire)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: &'static str, real: u64) -> Entry {
        Entry { kind, real, parent: 0, size: 0 }
    }

    #[test]
    fn register_translate_and_unregister() {
        let mut r = Registry::new();
        r.register(5, e("VkBuffer", 0xAAAA)).unwrap();
        assert_eq!(r.wire_to_handle("VkBuffer", 5), Ok(0xAAAA));
        assert_eq!(r.wire_to_handle("VkBuffer", 0), Ok(0), "VK_NULL_HANDLE");
        assert!(r.unregister(5).is_some());
        assert_eq!(r.wire_to_handle("VkBuffer", 5), Err(CodecError::Handle(5)));
    }

    #[test]
    fn wrong_kind_zero_duplicate_and_unknown_are_rejected() {
        let mut r = Registry::new();
        r.register(5, e("VkBuffer", 1)).unwrap();
        assert_eq!(r.wire_to_handle("VkImage", 5), Err(CodecError::Handle(5)), "bufor nie może udawać obrazu");
        assert_eq!(r.wire_to_handle("VkBuffer", 99), Err(CodecError::Handle(99)));
        assert!(r.register(5, e("VkImage", 2)).is_err(), "nie nadpisujemy wpisu");
        assert!(r.register(0, e("VkBuffer", 3)).is_err());
        assert_eq!(r.get(5).unwrap().real, 1);
    }

    #[test]
    fn limit_is_enforced() {
        let mut r = Registry::new();
        for i in 1..=MAX_HANDLES as u64 {
            r.register(i, e("VkBuffer", i)).unwrap();
        }
        assert!(r.register(u64::MAX, e("VkBuffer", 1)).is_err());
        assert_eq!(r.len(), MAX_HANDLES);
    }

    #[test]
    fn server_to_client_translation_is_refused() {
        assert!(Registry::new().handle_to_wire("VkBuffer", 7).is_err());
    }
}
