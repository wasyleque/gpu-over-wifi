//! Księgowość pul deskryptorów po stronie klienta: `vkAllocateDescriptorSets` może się nie powieść tylko przy wyczerpaniu puli
//! (maxSets albo liczby deskryptorów danego typu). Gdy wiemy, że pula starczy, odpowiadamy aplikacji od razu (bez obiegu po sieci);
//! w każdym niepewnym przypadku alokacja idzie synchronicznie i aplikacja dostaje prawdziwy wynik.

use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Layout {
    /// (typ deskryptora, liczba) zsumowane po wiązaniach.
    counts: Vec<(u32, u32)>,
    /// Zmienna liczba deskryptorów, flagi wiązań (pNext) albo typ, którego nie umiemy policzyć.
    special: bool,
}

#[derive(Debug, Clone)]
struct Pool {
    max_sets: u32,
    sizes: Vec<(u32, u32)>,
    sets_left: u32,
    left: HashMap<u32, u32>,
    /// Flaga zwalniania pojedynczych zestawów, rozszerzenia w pNext albo wcześniejsza odmowa: nie spekulujemy.
    special: bool,
    /// Diagnostyka: ile razy aplikacja zresetowała pulę i ile alokacji (wywołań) zrobiła od ostatniego resetu.
    resets: u64,
    allocs_since_reset: u64,
    /// Odmowy serwera od ostatniego resetu: normalny cykl aplikacji to jedna odmowa, potem reset; kolejne oznaczają, że reset nie pomógł.
    refused_since_reset: u64,
}

#[derive(Default)]
pub struct DescriptorAccounts {
    layouts: HashMap<u64, Layout>,
    pools: HashMap<u64, Pool>,
}

impl DescriptorAccounts {
    /// Zapamiętuje układ po `vkCreateDescriptorSetLayout` (`VkDescriptorSetLayoutCreateInfo`: pNext @8, bindingCount @20, pBindings @24).
    ///
    /// # Safety
    /// `info` musi wskazywać na poprawną strukturę z tablicą wiązań (24 B każde: typ @4, liczba @8).
    pub unsafe fn layout_created(&mut self, id: u64, info: *const u8) {
        let pnext = std::ptr::read_unaligned(info.add(8) as *const u64);
        let n = std::ptr::read_unaligned(info.add(20) as *const u32).min(4096) as usize;
        let b = std::ptr::read_unaligned(info.add(24) as *const u64) as *const u8;
        let mut counts: Vec<(u32, u32)> = Vec::new();
        let mut special = pnext != 0 || (n > 0 && b.is_null());
        for i in 0..if b.is_null() { 0 } else { n } {
            let ty = std::ptr::read_unaligned(b.add(i * 24 + 4) as *const u32);
            let c = std::ptr::read_unaligned(b.add(i * 24 + 8) as *const u32);
            // Typy z rozszerzeń (inline uniform block liczy bajty, mutable itd.) zostawiamy synchronicznie.
            if ty >= 1_000_000_000 {
                special = true;
            }
            match counts.iter_mut().find(|(t, _)| *t == ty) {
                Some(e) => e.1 = e.1.saturating_add(c),
                None => counts.push((ty, c)),
            }
        }
        self.layouts.insert(id, Layout { counts, special });
    }

    pub fn layout_destroyed(&mut self, id: u64) {
        self.layouts.remove(&id);
    }

    /// Zapamiętuje pulę po `vkCreateDescriptorPool` (flags @16, maxSets @20, poolSizeCount @24, pPoolSizes @32; pNext @8).
    ///
    /// # Safety
    /// `info` musi wskazywać na poprawną strukturę z tablicą rozmiarów (8 B każdy: typ, liczba).
    pub unsafe fn pool_created(&mut self, id: u64, info: *const u8) {
        let pnext = std::ptr::read_unaligned(info.add(8) as *const u64);
        let flags = std::ptr::read_unaligned(info.add(16) as *const u32);
        let max_sets = std::ptr::read_unaligned(info.add(20) as *const u32);
        let n = std::ptr::read_unaligned(info.add(24) as *const u32).min(256) as usize;
        let p = std::ptr::read_unaligned(info.add(32) as *const u64) as *const u8;
        let mut sizes: Vec<(u32, u32)> = Vec::new();
        if !p.is_null() {
            for i in 0..n {
                let ty = std::ptr::read_unaligned(p.add(i * 8) as *const u32);
                let c = std::ptr::read_unaligned(p.add(i * 8 + 4) as *const u32);
                match sizes.iter_mut().find(|(t, _)| *t == ty) {
                    Some(e) => e.1 = e.1.saturating_add(c),
                    None => sizes.push((ty, c)),
                }
            }
        }
        // Bezpieczna jest tylko pula bez flag poza UPDATE_AFTER_BIND (0x2): FREE_DESCRIPTOR_SET dopuszcza fragmentację.
        let special = pnext != 0 || flags & !0x2 != 0 || sizes.iter().any(|(t, _)| *t >= 1_000_000_000);
        let left = sizes.iter().copied().collect();
        self.pools.insert(id, Pool { max_sets, sizes, sets_left: max_sets, left, special, resets: 0, allocs_since_reset: 0, refused_since_reset: 0 });
    }

    pub fn pool_destroyed(&mut self, id: u64) {
        self.pools.remove(&id);
    }

    /// `vkResetDescriptorPool`: pula wraca do pełnej pojemności.
    pub fn pool_reset(&mut self, id: u64) {
        if let Some(p) = self.pools.get_mut(&id) {
            p.sets_left = p.max_sets;
            p.left = p.sizes.iter().copied().collect();
            p.resets += 1;
            p.allocs_since_reset = 0;
            p.refused_since_reset = 0;
        }
    }

    /// Czy alokacja `layouts` z puli `pool` na pewno się zmieści? Jeśli tak, odejmuje zasoby i zwraca `true` (można odpowiedzieć od razu).
    /// `has_pnext`: `VkDescriptorSetAllocateInfo` ma łańcuch pNext (np. zmienna liczba deskryptorów).
    pub fn try_allocate(&mut self, pool: u64, layouts: &[u64], has_pnext: bool) -> bool {
        if has_pnext || layouts.is_empty() {
            return false;
        }
        let Some(p) = self.pools.get_mut(&pool) else { return false };
        if p.special || p.sets_left < layouts.len() as u32 {
            return false;
        }
        let mut need: HashMap<u32, u32> = HashMap::new();
        for l in layouts {
            let Some(layout) = self.layouts.get(l) else { return false };
            if layout.special {
                return false;
            }
            for (t, c) in &layout.counts {
                *need.entry(*t).or_insert(0) += *c;
            }
        }
        if need.iter().any(|(t, c)| p.left.get(t).copied().unwrap_or(0) < *c) {
            return false;
        }
        p.sets_left -= layouts.len() as u32;
        for (t, c) in need {
            if let Some(v) = p.left.get_mut(&t) {
                *v -= c;
            }
        }
        true
    }

    /// Opis stanu naszej księgowości dla odmowy serwera (do logu): ile zestawów i deskryptorów każdego typu zostało wg nas w puli, ile
    /// potrzebowała alokacja oraz ile wynosi pojemność puli. Rozbieżność z faktyczną odmową = sterownik liczy inaczej niż API albo gra
    /// nie zrobiła resetu puli, którego oczekuje.
    pub fn explain(&self, pool: u64, layouts: &[u64]) -> String {
        let Some(p) = self.pools.get(&pool) else { return format!("pula {pool:#x} nieznana") };
        let mut need: Vec<(u32, u32)> = Vec::new();
        for l in layouts {
            match self.layouts.get(l) {
                Some(lay) => {
                    for (t, c) in &lay.counts {
                        match need.iter_mut().find(|(x, _)| x == t) {
                            Some(e) => e.1 += *c,
                            None => need.push((*t, *c)),
                        }
                    }
                }
                None => return format!("pula {pool:#x}: nieznany układ {l:#x}"),
            }
        }
        let mut left: Vec<(u32, u32)> = p.left.iter().map(|(t, c)| (*t, *c)).collect();
        left.sort_unstable();
        format!("pula {pool:#x}: maxSets {}, zostało wg klienta {} zestawów, deskryptory (typ,zostało) {:?}, pojemność {:?}, alokacja {} zestawów potrzebuje (typ,liczba) {:?}, pula specjalna: {}, resety puli: {}, alokacji od ostatniego resetu: {}", p.max_sets, p.sets_left, left, p.sizes, layouts.len(), need, p.special, p.resets, p.allocs_since_reset)
    }

    /// Po synchronicznej alokacji: serwer powiedział prawdę. Jeśli się udała i umiemy ją policzyć, odejmujemy zasoby jak przy spekulacji.
    /// Jeśli się nie udała ALBO się udała, ale nie umiemy jej policzyć (nieznany układ, pNext), serwer zużył albo wyczerpał pulę poza
    /// naszą księgowością, więc do resetu puli nie spekulujemy na niej już wcale.
    pub fn after_sync(&mut self, pool: u64, layouts: &[u64], ok: bool) {
        if let Some(p) = self.pools.get_mut(&pool) {
            p.allocs_since_reset += 1;
            if !ok {
                p.refused_since_reset += 1;
            }
        }
        let counted = ok && self.try_allocate(pool, layouts, false);
        if !counted {
            if let Some(p) = self.pools.get_mut(&pool) {
                p.sets_left = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_info(flags: u32, max_sets: u32, sizes: &[(u32, u32)], pnext: u64) -> (Vec<u8>, Vec<u32>) {
        let flat: Vec<u32> = sizes.iter().flat_map(|(t, c)| [*t, *c]).collect();
        let mut info = vec![0u8; 40];
        info[8..16].copy_from_slice(&pnext.to_le_bytes());
        info[16..20].copy_from_slice(&flags.to_le_bytes());
        info[20..24].copy_from_slice(&max_sets.to_le_bytes());
        info[24..28].copy_from_slice(&(sizes.len() as u32).to_le_bytes());
        info[32..40].copy_from_slice(&(flat.as_ptr() as u64).to_le_bytes());
        (info, flat)
    }

    fn layout_info(bindings: &[(u32, u32)], pnext: u64) -> (Vec<u8>, Vec<u8>) {
        let mut b = vec![0u8; bindings.len() * 24];
        for (i, (t, c)) in bindings.iter().enumerate() {
            b[i * 24 + 4..i * 24 + 8].copy_from_slice(&t.to_le_bytes());
            b[i * 24 + 8..i * 24 + 12].copy_from_slice(&c.to_le_bytes());
        }
        let mut info = vec![0u8; 32];
        info[8..16].copy_from_slice(&pnext.to_le_bytes());
        info[20..24].copy_from_slice(&(bindings.len() as u32).to_le_bytes());
        info[24..32].copy_from_slice(&(b.as_ptr() as u64).to_le_bytes());
        (info, b)
    }

    fn setup(pool_flags: u32, max_sets: u32, sizes: &[(u32, u32)]) -> DescriptorAccounts {
        let mut a = DescriptorAccounts::default();
        let (li, _lb) = layout_info(&[(1, 2), (6, 1)], 0);
        unsafe { a.layout_created(10, li.as_ptr()) };
        let (pi, _ps) = pool_info(pool_flags, max_sets, sizes, 0);
        unsafe { a.pool_created(7, pi.as_ptr()) };
        a
    }

    #[test]
    fn allocation_is_speculated_until_sets_or_descriptors_run_out() {
        let mut a = setup(0, 3, &[(1, 100), (6, 100)]);
        assert!(a.try_allocate(7, &[10], false));
        assert!(a.try_allocate(7, &[10, 10], false));
        assert!(!a.try_allocate(7, &[10], false), "maxSets = 3 wyczerpane");
        a.pool_reset(7);
        assert!(a.try_allocate(7, &[10], false), "po resecie znowu starcza");
    }

    #[test]
    fn running_out_of_one_descriptor_type_stops_speculation() {
        let mut a = setup(0, 100, &[(1, 5), (6, 100)]);
        assert!(a.try_allocate(7, &[10, 10], false)); // 4 z 5 deskryptorów typu 1
        assert!(!a.try_allocate(7, &[10], false), "brakuje deskryptorów typu 1");
    }

    #[test]
    fn unsure_cases_are_never_speculated() {
        let mut a = setup(1, 100, &[(1, 100), (6, 100)]);
        assert!(!a.try_allocate(7, &[10], false), "pula z FREE_DESCRIPTOR_SET");
        let mut b = setup(0, 100, &[(1, 100), (6, 100)]);
        let mut hosted = DescriptorAccounts::default();
        let (li2, _lb2) = layout_info(&[(1, 1)], 0);
        unsafe { hosted.layout_created(1, li2.as_ptr()) };
        let (pi2, _ps2) = pool_info(0x4, 10, &[(1, 10)], 0);
        unsafe { hosted.pool_created(2, pi2.as_ptr()) };
        assert!(!hosted.try_allocate(2, &[1], false), "pula z nieznaną flagą");
        assert!(!b.try_allocate(7, &[10], true), "pNext w alokacji (np. zmienna liczba)");
        assert!(!b.try_allocate(7, &[999], false), "nieznany układ");
        assert!(!b.try_allocate(8, &[10], false), "nieznana pula");
        let (li, _lb) = layout_info(&[(1, 2)], 0xDEAD);
        unsafe { b.layout_created(11, li.as_ptr()) };
        assert!(!b.try_allocate(7, &[11], false), "układ z pNext (flagi wiązań)");
        let (pi, _ps) = pool_info(0, 10, &[(1, 10)], 0x1234);
        unsafe { b.pool_created(9, pi.as_ptr()) };
        assert!(!b.try_allocate(9, &[10], false), "pula z pNext");
    }

    #[test]
    fn an_uncounted_successful_allocation_stops_speculation_on_that_pool_until_reset() {
        let mut a = setup(0, 100, &[(1, 100), (6, 100)]);
        // Układ 999 jest nieznany: serwer go zaalokował, ale nie umiemy odjąć zasobów.
        a.after_sync(7, &[999], true);
        assert!(!a.try_allocate(7, &[10], false), "liczniki nie są już wiarygodne");
        a.pool_reset(7);
        assert!(a.try_allocate(7, &[10], false), "po resecie pula jest znowu w pełni znana");
    }

    #[test]
    fn a_refusal_from_the_server_stops_speculation_until_reset() {
        let mut a = setup(0, 100, &[(1, 100), (6, 100)]);
        a.after_sync(7, &[10], false);
        assert!(!a.try_allocate(7, &[10], false));
        a.pool_reset(7);
        assert!(a.try_allocate(7, &[10], false));
        a.after_sync(7, &[10], true);
        a.pool_destroyed(7);
        assert!(!a.try_allocate(7, &[10], false));
    }
}
