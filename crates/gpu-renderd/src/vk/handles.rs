//! Tablica uchwytów z generacją: klient dostaje nieprzejrzysty `u64`, nigdy adresu serwera.
//! Stary lub sfałszowany uchwyt daje `None`, nie panikę. Uchwyt 0 (VK_NULL_HANDLE) nigdy nie jest ważny.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle(pub u64);

impl Handle {
    pub fn raw(self) -> u64 {
        self.0
    }
    pub fn from_raw(v: u64) -> Handle {
        Handle(v)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct TableFull;

#[derive(Debug)]
struct Slot<T> {
    gen: u32,
    val: Option<T>,
    next_free: u32,
}

pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    free_head: u32,
    /// Globalna generacja: rośnie przy każdym `insert`, nigdy nie przyjmuje wartości 0.
    generation: u32,
    max: usize,
    live: usize,
}

impl<T> HandleTable<T> {
    /// Tablica bez limitu (dla zaufanych wywołań).
    pub fn new() -> Self {
        Self::with_limit(usize::MAX)
    }

    /// Tablica, która przyjmie co najwyżej `max` jednocześnie żywych wpisów.
    pub fn with_limit(max: usize) -> Self {
        HandleTable { slots: Vec::new(), free_head: u32::MAX, generation: 0, max, live: 0 }
    }

    /// Wyłącznie do testów: generacja, od której zaczyna się liczenie.
    #[cfg(test)]
    pub(crate) fn with_generation(max: usize, generation: u32) -> Self {
        HandleTable { generation, ..Self::with_limit(max) }
    }

    fn next_generation(&mut self) -> u32 {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }
        self.generation
    }

    /// Wstawia wartość i zwraca świeży uchwyt (nigdy 0, nigdy równy żadnemu wcześniej wydanemu
    /// uchwytowi żywego wpisu). `Err(TableFull)` gdy limit osiągnięty.
    pub fn insert(&mut self, v: T) -> Result<Handle, TableFull> {
        if self.live >= self.max {
            return Err(TableFull);
        }
        let reuse = if self.free_head != u32::MAX { Some(self.free_head) } else { None };
        let index = match reuse {
            Some(i) => i,
            None => {
                if self.slots.len() >= (u32::MAX - 1) as usize {
                    return Err(TableFull);
                }
                self.slots.len() as u32
            }
        };
        let gen = self.next_generation();
        match reuse {
            Some(i) => {
                let slot = self.slots.get_mut(i as usize).ok_or(TableFull)?;
                self.free_head = slot.next_free;
                *slot = Slot { gen, val: Some(v), next_free: u32::MAX };
            }
            None => self.slots.push(Slot { gen, val: Some(v), next_free: u32::MAX }),
        }
        self.live += 1;
        Ok(Handle(((gen as u64) << 32) | index as u64))
    }

    fn slot(&self, h: Handle) -> Option<&Slot<T>> {
        let slot = self.slots.get((h.0 & 0xFFFF_FFFF) as usize)?;
        (slot.gen == (h.0 >> 32) as u32 && slot.val.is_some()).then_some(slot)
    }

    pub fn get(&self, h: Handle) -> Option<&T> {
        self.slot(h)?.val.as_ref()
    }

    pub fn get_mut(&mut self, h: Handle) -> Option<&mut T> {
        self.slot(h)?;
        self.slots.get_mut((h.0 & 0xFFFF_FFFF) as usize)?.val.as_mut()
    }

    /// Usuwa wpis; drugie usunięcie tego samego uchwytu zwraca `None`.
    pub fn remove(&mut self, h: Handle) -> Option<T> {
        self.slot(h)?;
        let index = (h.0 & 0xFFFF_FFFF) as u32;
        let free_head = self.free_head;
        let slot = self.slots.get_mut(index as usize)?;
        let v = slot.val.take()?;
        slot.next_free = free_head;
        self.free_head = index;
        self.live -= 1;
        Some(v)
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Zwraca wszystkie żywe wartości (w kolejności slotów) i unieważnia każdy uchwyt.
    pub fn drain_all(&mut self) -> Vec<T> {
        let out: Vec<T> = self.slots.drain(..).filter_map(|s| s.val).collect();
        self.free_head = u32::MAX;
        self.live = 0;
        out
    }
}

impl<T> Default for HandleTable<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove_roundtrip() {
        let mut t = HandleTable::new();
        let a = t.insert("a").unwrap();
        let b = t.insert("b").unwrap();
        assert_ne!(a, b);
        assert_eq!((t.get(a), t.get(b)), (Some(&"a"), Some(&"b")));
        assert_eq!(t.len(), 2);
        assert_eq!(t.remove(a), Some("a"));
        assert_eq!(t.get(a), None);
        assert_eq!(t.get(b), Some(&"b"));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn handle_zero_is_never_issued_or_valid() {
        let mut t = HandleTable::new();
        for i in 0..100 {
            assert_ne!(t.insert(i).unwrap().raw(), 0);
        }
        assert_eq!(t.get(Handle(0)), None);
        assert_eq!(t.remove(Handle(0)), None);
    }

    #[test]
    fn stale_handle_is_rejected_even_when_slot_is_reused() {
        let mut t = HandleTable::new();
        let old = t.insert(1).unwrap();
        t.remove(old);
        let fresh = t.insert(2).unwrap();
        assert_ne!(old, fresh, "ponownie użyty slot musi dostać inną generację");
        assert_eq!(t.get(old), None);
        assert_eq!(t.get_mut(old), None);
        assert_eq!(t.remove(old), None);
        assert_eq!(t.get(fresh), Some(&2));
    }

    #[test]
    fn double_remove_returns_none() {
        let mut t = HandleTable::new();
        let h = t.insert(5).unwrap();
        assert_eq!(t.remove(h), Some(5));
        assert_eq!(t.remove(h), None);
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn get_mut_modifies_in_place() {
        let mut t = HandleTable::new();
        let h = t.insert(String::from("x")).unwrap();
        t.get_mut(h).unwrap().push('y');
        assert_eq!(t.get(h).unwrap(), "xy");
    }

    #[test]
    fn forged_handles_never_panic_and_never_match() {
        let mut t = HandleTable::new();
        let real = t.insert(1u8).unwrap();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..10_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let h = Handle(x);
            if h != real {
                assert_eq!(t.get(h), None, "{x:#x}");
                assert_eq!(t.remove(h), None);
            }
        }
        for h in [Handle(u64::MAX), Handle(1 << 32), Handle(u32::MAX as u64), Handle(0xdead_beef)] {
            if h != real {
                assert_eq!(t.get(h), None);
            }
        }
        assert_eq!(t.get(real), Some(&1));
    }

    #[test]
    fn limit_is_enforced_and_freed_by_remove() {
        let mut t = HandleTable::with_limit(3);
        let hs: Vec<_> = (0..3).map(|i| t.insert(i).unwrap()).collect();
        assert_eq!(t.insert(9), Err(TableFull));
        assert_eq!(t.len(), 3);
        t.remove(hs[1]);
        assert!(t.insert(9).is_ok());
        assert_eq!(t.insert(10), Err(TableFull));
    }

    #[test]
    fn drain_all_returns_everything_and_invalidates_handles() {
        let mut t = HandleTable::new();
        let hs: Vec<_> = (0..5).map(|i| t.insert(i).unwrap()).collect();
        t.remove(hs[2]);
        let mut all = t.drain_all();
        all.sort();
        assert_eq!(all, vec![0, 1, 3, 4]);
        assert!(t.is_empty());
        for h in hs {
            assert_eq!(t.get(h), None);
        }
        assert!(t.insert(7).is_ok(), "po drain tablica dalej działa");
    }

    #[test]
    fn generation_wraparound_skips_zero() {
        // Generacja tuż przed zawinięciem: żaden wydany uchwyt nie może mieć generacji 0.
        let mut t = HandleTable::with_generation(usize::MAX, u32::MAX - 2);
        let mut seen = std::collections::HashSet::new();
        for i in 0..10 {
            let h = t.insert(i).unwrap();
            assert_ne!(h.raw() >> 32, 0, "generacja 0 zabroniona: {h:?}");
            assert!(seen.insert(h));
            t.remove(h);
        }
    }

    #[test]
    fn many_inserts_and_removes_stay_consistent() {
        let mut t = HandleTable::new();
        let mut live: Vec<(Handle, u32)> = Vec::new();
        let mut x: u32 = 12345;
        for step in 0..5000u32 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            if !x.is_multiple_of(3) || live.is_empty() {
                live.push((t.insert(step).unwrap(), step));
            } else {
                let i = (x as usize / 7) % live.len();
                let (h, v) = live.swap_remove(i);
                assert_eq!(t.remove(h), Some(v));
            }
        }
        assert_eq!(t.len(), live.len());
        for (h, v) in &live {
            assert_eq!(t.get(*h), Some(v));
        }
    }
}
