//! Lokalne wyliczanie deskryptorów buforów (`vkGetDescriptorEXT` dla UNIFORM_BUFFER i STORAGE_BUFFER).
//!
//! Gra (vkd3d-proton) pyta o deskryptor bufora stałego przy każdym rysowaniu z nowym (adres, zakres), więc cache nie pomaga, a każde
//! pytanie to obieg po sieci (90% chybień w Cyberpunku). Na karcie NVIDIA (sterownik 580) bajty deskryptora są prostą funkcją
//! `(adres, zakres)`, zmierzoną sondą na prawdziwym sterowniku (`gpu-renderd/tests/descriptor_probe.rs`, 2249 z 2249 próbek):
//!
//! - UNIFORM_BUFFER, 8 B: `0` gdy adres lub zakres = 0; inaczej `(adres >> 4)` w bitach 0..44 i `((zakres + 15) >> 4) << 1` (20 bitów)
//!   w bitach 44..64 (little endian). Format i dolne 4 bity adresu nie wpływają na rozmiar.
//! - STORAGE_BUFFER, 16 B: `0` gdy adres lub zakres = 0; inaczej `adres` (u64) i `zakres` (u64).
//!
//! To zachowanie konkretnego sterownika, więc NIE jest przyjmowane na wiarę: wzór włącza się dopiero po `LEARN_NEEDED` kolejnych
//! odpowiedziach serwera zgodnych z wzorem, potem co `AUDIT_EVERY`-te wywołanie idzie normalnie do serwera i jest porównywane,
//! a pierwsza niezgodność wyłącza mechanizm na stałe (wszystkie deskryptory wracają do zapytań).

pub const UNIFORM_BUFFER: u32 = 6;
pub const STORAGE_BUFFER: u32 = 7;
/// Tyle kolejnych zgodnych (i nietrywialnych) odpowiedzi serwera potrzeba, żeby zacząć liczyć lokalnie.
pub const LEARN_NEEDED: u32 = 32;
/// Co które wywołanie po włączeniu idzie jednak do serwera, żeby sprawdzić, że wzór nadal obowiązuje.
pub const AUDIT_EVERY: u64 = 512;

/// Oczekiwany rozmiar deskryptora danego typu.
pub fn size_of(ty: u32) -> Option<usize> {
    match ty {
        UNIFORM_BUFFER => Some(8),
        STORAGE_BUFFER => Some(16),
        _ => None,
    }
}

/// Bajty deskryptora według wzoru (patrz opis modułu); `None` dla typów, których nie liczymy.
pub fn encode(ty: u32, address: u64, range: u64) -> Option<Vec<u8>> {
    let size = size_of(ty)?;
    if address == 0 || range == 0 {
        return Some(vec![0; size]);
    }
    Some(match ty {
        UNIFORM_BUFFER => {
            let base = (address >> 4) & ((1u64 << 44) - 1);
            let size_field = (((range + 15) >> 4) << 1) & 0xF_FFFF;
            (base | (size_field << 44)).to_le_bytes().to_vec()
        }
        _ => {
            let mut v = address.to_le_bytes().to_vec();
            v.extend_from_slice(&range.to_le_bytes());
            v
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Learning(u32),
    Verified(u64),
    Disabled,
}

/// Co się zmieniło po porównaniu z odpowiedzią serwera (do jednorazowego komunikatu w logu).
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    None,
    Verified,
    Disabled,
}

/// Stan mechanizmu dla obu typów (indeks 0: UNIFORM_BUFFER, 1: STORAGE_BUFFER).
pub struct Synth {
    mode: [Mode; 2],
}

impl Default for Synth {
    fn default() -> Synth {
        Synth { mode: [Mode::Learning(0); 2] }
    }
}

fn slot(ty: u32) -> Option<usize> {
    match ty {
        UNIFORM_BUFFER => Some(0),
        STORAGE_BUFFER => Some(1),
        _ => None,
    }
}

impl Synth {
    /// Deskryptor policzony lokalnie, jeśli wzór jest sprawdzony i to nie kolej na kontrolę; inaczej `None` (pytamy serwer).
    pub fn try_local(&mut self, ty: u32, size: usize, address: u64, range: u64) -> Option<Vec<u8>> {
        let i = slot(ty)?;
        if size_of(ty) != Some(size) {
            return None;
        }
        match &mut self.mode[i] {
            Mode::Verified(calls) => {
                *calls += 1;
                if *calls % AUDIT_EVERY == 0 {
                    return None;
                }
                encode(ty, address, range)
            }
            _ => None,
        }
    }

    /// Porównuje prawdziwą odpowiedź serwera ze wzorem (uczenie i kontrola); wywoływane po każdym zapytaniu o taki deskryptor.
    pub fn observe(&mut self, ty: u32, address: u64, range: u64, actual: &[u8]) -> Event {
        let Some(i) = slot(ty) else { return Event::None };
        let Some(expected) = encode(ty, address, range) else { return Event::None };
        if expected.len() != actual.len() {
            return Event::None;
        }
        match self.mode[i] {
            Mode::Disabled => Event::None,
            Mode::Learning(n) => {
                if expected != actual {
                    self.mode[i] = Mode::Disabled;
                    return Event::Disabled;
                }
                // Wejścia zerowe zgadzają się trywialnie (same zera): nie liczą się do nauki.
                if address == 0 || range == 0 {
                    return Event::None;
                }
                if n + 1 >= LEARN_NEEDED {
                    self.mode[i] = Mode::Verified(0);
                    Event::Verified
                } else {
                    self.mode[i] = Mode::Learning(n + 1);
                    Event::None
                }
            }
            Mode::Verified(_) => {
                if expected != actual {
                    self.mode[i] = Mode::Disabled;
                    Event::Disabled
                } else {
                    Event::None
                }
            }
        }
    }
}

/// Wejście `vkGetDescriptorEXT` potrzebne do wzoru: `(typ, adres, zakres)` z `VkDescriptorGetInfoEXT`, o ile to deskryptor bufora
/// z poprawnym wskaźnikiem na `VkDescriptorAddressInfoEXT` (adres @16, zakres @24).
///
/// # Safety
/// `info` musi być zerem albo wskazywać na poprawne `VkDescriptorGetInfoEXT` (struktury, na które wskazuje, też muszą być ważne).
pub unsafe fn input_of(info: *const u8) -> Option<(u32, u64, u64)> {
    if info.is_null() {
        return None;
    }
    let ty = std::ptr::read_unaligned(info.add(16) as *const u32);
    slot(ty)?;
    let p = std::ptr::read_unaligned(info.add(24) as *const *const u8);
    if p.is_null() {
        return None;
    }
    Some((ty, std::ptr::read_unaligned(p.add(16) as *const u64), std::ptr::read_unaligned(p.add(24) as *const u64)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Wektory zmierzone na RTX 3090 Ti (sterownik 580.178) sondą `descriptor_probe`.
    #[test]
    fn the_formulas_reproduce_descriptors_measured_on_the_real_driver() {
        let ubo = [
            (0x1000u64, 0x100u64, "0001000000000200"),
            (0x228ee7b18110, 0x55bcbb, "11187bee288279ab"),
            (0x572bfb805a14, 0x786daa, "a105b8bf7265dbf0"),
            (0x46a67d004c04, 0xbcf366, "c004d0676ae4e679"),
            (0x5fb874e95114, 0xdedfd3, "11954e87fbc5bfbd"),
            (0x39b16b714b3c, 0xae9a1a, "b314b7169b43345d"),
        ];
        for (a, r, h) in ubo {
            assert_eq!(encode(UNIFORM_BUFFER, a, r).unwrap(), hex(h), "UBO {a:#x} {r:#x}");
        }
        let ssbo = [
            (0x1000u64, 0x100u64, "00100000000000000001000000000000"),
            (0x6e1e6ced8118, 0x98f7f6, "1881ed6c1e6e0000f6f7980000000000"),
            (0x5237f307d930, 0xe763e6, "30d907f337520000e663e70000000000"),
            (0x11499417aa0c, 0xda0bad, "0caa179449110000ad0bda0000000000"),
            (0x220196711d18, 0xea4aae, "181d719601220000ae4aea0000000000"),
            (0x53635fd8a910, 0x1cc33f, "10a9d85f635300003fc31c0000000000"),
        ];
        for (a, r, h) in ssbo {
            assert_eq!(encode(STORAGE_BUFFER, a, r).unwrap(), hex(h), "SSBO {a:#x} {r:#x}");
        }
        // Zerowy adres albo zakres daje deskryptor zerowy; inne typy nie są liczone.
        assert_eq!(encode(UNIFORM_BUFFER, 0, 5).unwrap(), vec![0; 8]);
        assert_eq!(encode(STORAGE_BUFFER, 0x1000, 0).unwrap(), vec![0; 16]);
        assert!(encode(2, 0x1000, 16).is_none());
    }

    #[test]
    fn local_answers_start_only_after_enough_matching_server_answers_and_audit_periodically() {
        let mut s = Synth::default();
        assert!(s.try_local(UNIFORM_BUFFER, 8, 0x1000, 256).is_none(), "na początku pytamy serwer");
        // Trywialne wejścia (zera) nie liczą się do nauki.
        for _ in 0..100 {
            assert_eq!(s.observe(UNIFORM_BUFFER, 0, 0, &[0; 8]), Event::None);
        }
        assert!(s.try_local(UNIFORM_BUFFER, 8, 0x1000, 256).is_none());
        for i in 0..LEARN_NEEDED {
            let a = 0x7000_0000_0000u64 + i as u64 * 0x100;
            let ev = s.observe(UNIFORM_BUFFER, a, 256, &encode(UNIFORM_BUFFER, a, 256).unwrap());
            assert_eq!(ev, if i + 1 == LEARN_NEEDED { Event::Verified } else { Event::None });
        }
        // Teraz lokalnie, ale co AUDIT_EVERY-te wywołanie idzie do serwera; typ STORAGE uczy się osobno.
        let mut local = 0;
        let mut asked = 0;
        for i in 0..AUDIT_EVERY * 2 {
            match s.try_local(UNIFORM_BUFFER, 8, 0x1000 + i * 16, 64) {
                Some(d) => {
                    assert_eq!(d, encode(UNIFORM_BUFFER, 0x1000 + i * 16, 64).unwrap());
                    local += 1;
                }
                None => asked += 1,
            }
        }
        assert_eq!((local, asked), (AUDIT_EVERY * 2 - 2, 2));
        assert!(s.try_local(STORAGE_BUFFER, 16, 0x1000, 64).is_none(), "drugi typ jeszcze się uczy");
        // Zły rozmiar nigdy nie jest liczony lokalnie.
        assert!(s.try_local(UNIFORM_BUFFER, 16, 0x1000, 64).is_none());
    }

    #[test]
    fn a_single_disagreement_disables_the_mechanism_permanently() {
        let mut s = Synth::default();
        for i in 0..LEARN_NEEDED {
            let a = 0x1000 + i as u64 * 0x100;
            s.observe(STORAGE_BUFFER, a, 64, &encode(STORAGE_BUFFER, a, 64).unwrap());
        }
        assert!(s.try_local(STORAGE_BUFFER, 16, 0x9000, 64).is_some());
        // Serwer odpowiada inaczej niż wzór (np. inna wersja sterownika): koniec liczenia lokalnego.
        assert_eq!(s.observe(STORAGE_BUFFER, 0x9000, 64, &[1; 16]), Event::Disabled);
        assert!(s.try_local(STORAGE_BUFFER, 16, 0x9000, 64).is_none());
        for i in 0..LEARN_NEEDED * 2 {
            let a = 0x1000 + i as u64 * 0x100;
            s.observe(STORAGE_BUFFER, a, 64, &encode(STORAGE_BUFFER, a, 64).unwrap());
        }
        assert!(s.try_local(STORAGE_BUFFER, 16, 0x9000, 64).is_none(), "wyłączenie jest trwałe");
        // Niezgodność już w fazie uczenia też wyłącza.
        let mut t = Synth::default();
        assert_eq!(t.observe(UNIFORM_BUFFER, 0x1000, 64, &[9; 8]), Event::Disabled);
    }

    #[test]
    fn input_is_read_from_the_descriptor_get_info_layout() {
        // VkDescriptorGetInfoEXT: type@16, data@24 (wskaźnik); VkDescriptorAddressInfoEXT: address@16, range@24.
        let mut addr = [0u64; 5];
        addr[2] = 0xAABB_0000;
        addr[3] = 4096;
        let mut info = [0u64; 4];
        info[2] = UNIFORM_BUFFER as u64;
        info[3] = addr.as_ptr() as u64;
        assert_eq!(unsafe { input_of(info.as_ptr() as *const u8) }, Some((UNIFORM_BUFFER, 0xAABB_0000, 4096)));
        info[2] = 2; // SAMPLED_IMAGE: nie liczymy
        assert_eq!(unsafe { input_of(info.as_ptr() as *const u8) }, None);
        info[2] = STORAGE_BUFFER as u64;
        info[3] = 0;
        assert_eq!(unsafe { input_of(info.as_ptr() as *const u8) }, None, "pusty wskaźnik");
        assert_eq!(unsafe { input_of(std::ptr::null()) }, None);
    }
}
