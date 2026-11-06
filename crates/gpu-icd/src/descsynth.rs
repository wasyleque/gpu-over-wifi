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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Gotowość wzoru i licznik wywołań dla audytu, dostępne BEZ blokady instancji (wywołań `vkGetDescriptorEXT` są miliony): ustawiane przy
/// zmianie stanu w `observe`, czytane w `try_lockfree`. Stan jest jeden na proces (jak sterownik NVIDIA, od którego zależy wzór).
static READY: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
static CALLS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

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

    /// Jak `try_local`, ale bez blokady i bez `&mut self`: korzysta tylko ze statycznych flag (patrz `READY`).
    pub fn try_lockfree(ty: u32, size: usize, address: u64, range: u64) -> Option<Vec<u8>> {
        let i = slot(ty)?;
        if size_of(ty) != Some(size) || !READY[i].load(Ordering::Acquire) {
            return None;
        }
        if CALLS[i].fetch_add(1, Ordering::Relaxed) % AUDIT_EVERY == AUDIT_EVERY - 1 {
            return None; // co AUDIT_EVERY-te wywołanie idzie do serwera i jest porównywane (`observe`)
        }
        encode(ty, address, range)
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
                    READY[i].store(false, Ordering::Release);
                    return Event::Disabled;
                }
                // Wejścia zerowe zgadzają się trywialnie (same zera): nie liczą się do nauki.
                if address == 0 || range == 0 {
                    return Event::None;
                }
                if n + 1 >= LEARN_NEEDED {
                    self.mode[i] = Mode::Verified(0);
                    READY[i].store(true, Ordering::Release);
                    Event::Verified
                } else {
                    self.mode[i] = Mode::Learning(n + 1);
                    Event::None
                }
            }
            Mode::Verified(_) => {
                if expected != actual {
                    self.mode[i] = Mode::Disabled;
                    READY[i].store(false, Ordering::Release);
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

// ---------- bufory teksela (UNIFORM_TEXEL_BUFFER = 4, STORAGE_TEXEL_BUFFER = 5) ----------
//
// Sonda `descriptor_probe` (1008 próbek: 18 formatów x 7 zakresów x 4 adresy, RTX 3090 Ti, sterownik 580; surowe dane w
// `docs/measurements/descriptor-probes/`) pokazała, że 16-bajtowy deskryptor to dla 17 z 18 formatów: `adres >> k` (u64), liczba elementów
// `zakres / 2^k` (u32, w dół) i stała zależna tylko od formatu (u32); typy 4 i 5 dają te same bajty. Format R32G32B32 (12-bajtowy teksel, 106) nie
// ma takiej postaci (dzielenie przez 12 i zależność od wyrównania), więc zostaje na zapytaniach do serwera. Jak przy buforach, wzór NIE jest
// przyjmowany na wiarę: włącza się po `LEARN_NEEDED` zgodnych odpowiedziach serwera dla danego formatu, jest audytowany, a niezgodność wyłącza format na stałe.

pub const UNIFORM_TEXEL_BUFFER: u32 = 4;
pub const STORAGE_TEXEL_BUFFER: u32 = 5;

/// `(VkFormat, k, stała)`: przesunięcie adresu i słowo trzecie deskryptora (zmierzone sondą).
const TEXEL_FORMATS: [(u32, u32, u32); 17] = [
    (9, 0, 653),
    (13, 0, 3858),
    (37, 2, 4566),
    (43, 2, 2582),
    (44, 2, 1039),
    (50, 2, 2517),
    (64, 2, 4631),
    (70, 1, 1169),
    (74, 1, 3729),
    (76, 1, 2323),
    (81, 2, 3243),
    (83, 2, 2258),
    (97, 3, 2225),
    (100, 2, 2160),
    (103, 3, 2127),
    (109, 4, 2041),
    (122, 2, 2712),
];

static TEXEL_READY: [AtomicBool; 17] = [const { AtomicBool::new(false) }; 17];
static TEXEL_CALLS: [AtomicU64; 17] = [const { AtomicU64::new(0) }; 17];

fn texel_slot(format: u32) -> Option<usize> {
    TEXEL_FORMATS.iter().position(|f| f.0 == format)
}

/// Bajty deskryptora teksela według wzoru; `None` dla formatów spoza tabeli i dla wejść zerowych (te zostają na serwerze).
pub fn texel_encode(address: u64, range: u64, format: u32) -> Option<[u8; 16]> {
    let (_, k, constant) = TEXEL_FORMATS[texel_slot(format)?];
    if address == 0 || range == 0 {
        return None;
    }
    let mut out = [0u8; 16];
    out[0..8].copy_from_slice(&(address >> k).to_le_bytes());
    out[8..12].copy_from_slice(&((range >> k) as u32).to_le_bytes());
    out[12..16].copy_from_slice(&constant.to_le_bytes());
    Some(out)
}

/// Wejście `vkGetDescriptorEXT` dla bufora teksela: `(adres, zakres, format)` z `VkDescriptorAddressInfoEXT` (adres @16, zakres @24, format @32).
///
/// # Safety
/// Jak `input_of`.
pub unsafe fn texel_input_of(info: *const u8) -> Option<(u64, u64, u32)> {
    if info.is_null() {
        return None;
    }
    let ty = std::ptr::read_unaligned(info.add(16) as *const u32);
    if ty != UNIFORM_TEXEL_BUFFER && ty != STORAGE_TEXEL_BUFFER {
        return None;
    }
    let p = std::ptr::read_unaligned(info.add(24) as *const *const u8);
    if p.is_null() {
        return None;
    }
    Some((std::ptr::read_unaligned(p.add(16) as *const u64), std::ptr::read_unaligned(p.add(24) as *const u64), std::ptr::read_unaligned(p.add(32) as *const u32)))
}

/// Stan uczenia formatów tekseli (po jednym na format z tabeli; wspólny dla typów 4 i 5).
pub struct TexelSynth {
    mode: [Mode; 17],
}

impl Default for TexelSynth {
    fn default() -> TexelSynth {
        TexelSynth { mode: [Mode::Learning(0); 17] }
    }
}

impl TexelSynth {
    /// Deskryptor policzony lokalnie, jeśli wzór dla formatu jest sprawdzony i to nie kolej na kontrolę; inaczej `None` (pytamy serwer).
    pub fn try_local(&mut self, size: usize, address: u64, range: u64, format: u32) -> Option<[u8; 16]> {
        let i = texel_slot(format)?;
        if size != 16 {
            return None;
        }
        match &mut self.mode[i] {
            Mode::Verified(calls) => {
                *calls += 1;
                if *calls % AUDIT_EVERY == 0 {
                    return None;
                }
                texel_encode(address, range, format)
            }
            _ => None,
        }
    }

    /// Jak `try_local`, ale bez blokady i bez `&mut self` (statyczne flagi gotowości).
    pub fn try_lockfree(size: usize, address: u64, range: u64, format: u32) -> Option<[u8; 16]> {
        let i = texel_slot(format)?;
        if size != 16 || !TEXEL_READY[i].load(Ordering::Acquire) {
            return None;
        }
        if TEXEL_CALLS[i].fetch_add(1, Ordering::Relaxed) % AUDIT_EVERY == AUDIT_EVERY - 1 {
            return None;
        }
        texel_encode(address, range, format)
    }

    /// Porównuje prawdziwą odpowiedź serwera ze wzorem (uczenie i kontrola).
    pub fn observe(&mut self, address: u64, range: u64, format: u32, actual: &[u8]) -> Event {
        let Some(i) = texel_slot(format) else { return Event::None };
        let Some(expected) = texel_encode(address, range, format) else { return Event::None };
        let ok = expected[..] == *actual;
        match self.mode[i] {
            Mode::Disabled => Event::None,
            Mode::Learning(n) => {
                if !ok {
                    self.mode[i] = Mode::Disabled;
                    TEXEL_READY[i].store(false, Ordering::Release);
                    return Event::Disabled;
                }
                if n + 1 >= LEARN_NEEDED {
                    self.mode[i] = Mode::Verified(0);
                    TEXEL_READY[i].store(true, Ordering::Release);
                    Event::Verified
                } else {
                    self.mode[i] = Mode::Learning(n + 1);
                    Event::None
                }
            }
            Mode::Verified(_) => {
                if ok {
                    Event::None
                } else {
                    self.mode[i] = Mode::Disabled;
                    TEXEL_READY[i].store(false, Ordering::Release);
                    Event::Disabled
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Testy współdzielą statyczny stan gotowości (`READY`), więc nie mogą biec równolegle.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
    #[test]
    fn lockfree_answers_follow_the_published_readiness_and_audit_every_nth_call() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        READY[1].store(false, Ordering::SeqCst);
        CALLS[1].store(0, Ordering::SeqCst);
        assert!(Synth::try_lockfree(STORAGE_BUFFER, 16, 0x1000, 64).is_none(), "przed potwierdzeniem pytamy serwer");
        let mut s = Synth::default();
        for i in 0..LEARN_NEEDED {
            let a = 0x1000 + i as u64 * 0x100;
            s.observe(STORAGE_BUFFER, a, 64, &encode(STORAGE_BUFFER, a, 64).unwrap());
        }
        assert!(READY[1].load(Ordering::SeqCst));
        let (mut local, mut asked) = (0, 0);
        for i in 0..AUDIT_EVERY * 2 {
            match Synth::try_lockfree(STORAGE_BUFFER, 16, 0x1000 + i * 16, 64) {
                Some(d) => {
                    assert_eq!(d, encode(STORAGE_BUFFER, 0x1000 + i * 16, 64).unwrap());
                    local += 1;
                }
                None => asked += 1,
            }
        }
        assert_eq!((local, asked), (AUDIT_EVERY * 2 - 2, 2));
        assert!(Synth::try_lockfree(STORAGE_BUFFER, 8, 0x1000, 64).is_none(), "zły rozmiar");
        // Niezgodność wyłącza także ścieżkę bez blokady.
        assert_eq!(s.observe(STORAGE_BUFFER, 0x9000, 64, &[1; 16]), Event::Disabled);
        assert!(Synth::try_lockfree(STORAGE_BUFFER, 16, 0x9000, 64).is_none());
    }

    /// Wszystkie próbki z sondy (1008 wierszy): formaty z tabeli są odtwarzane co do bajtu, a R32G32B32 (106) nie jest liczony lokalnie.
    #[test]
    fn texel_formulas_reproduce_every_probe_sample_of_the_declared_formats() {
        let probe = include_str!("../../../docs/measurements/descriptor-probes/texel-buffer-rtx3090ti-580.txt");
        let (mut checked, mut skipped) = (0, 0);
        for line in probe.lines().filter(|l| l.starts_with("TEXEL ")) {
            let p: Vec<&str> = line.split_whitespace().collect();
            let a = u64::from_str_radix(p[2].trim_start_matches("0x"), 16).unwrap();
            let r = u64::from_str_radix(p[3].trim_start_matches("0x"), 16).unwrap();
            let f: u32 = p[4].parse().unwrap();
            match texel_encode(a, r, f) {
                Some(got) => {
                    assert_eq!(got.to_vec(), hex(p[5]), "typ {} adres {a:#x} zakres {r:#x} format {f}", p[1]);
                    checked += 1;
                }
                None => {
                    assert_eq!(f, 106, "tylko format 106 zostaje na serwerze");
                    skipped += 1;
                }
            }
        }
        assert_eq!((checked, skipped), (952, 56), "17 formatów x 56 próbek i 56 próbek formatu 106");
    }

    #[test]
    fn texel_synthesis_learns_per_format_audits_and_disables_on_a_mismatch() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = TexelSynth::default();
        assert!(s.try_local(16, 0x1000, 256, 37).is_none(), "na początku pytamy serwer");
        for i in 0..LEARN_NEEDED as u64 {
            let a = 0x1000 + i * 16;
            let ev = s.observe(a, 256, 37, &texel_encode(a, 256, 37).unwrap());
            assert_eq!(ev, if i + 1 == LEARN_NEEDED as u64 { Event::Verified } else { Event::None });
        }
        assert_eq!(s.try_local(16, 0x2000, 64, 37), texel_encode(0x2000, 64, 37));
        assert!(s.try_local(16, 0x2000, 64, 109).is_none(), "inny format uczy się osobno");
        assert!(s.try_local(8, 0x2000, 64, 37).is_none(), "zły rozmiar deskryptora");
        assert!(texel_encode(0x2000, 64, 106).is_none() && texel_encode(0, 64, 37).is_none());
        assert_eq!(s.observe(0x2000, 64, 37, &[1; 16]), Event::Disabled);
        assert!(s.try_local(16, 0x2000, 64, 37).is_none(), "wyłączenie jest trwałe");
    }

    #[test]
    fn texel_input_is_read_from_the_descriptor_get_info_layout() {
        let mut addr = [0u8; 40];
        addr[16..24].copy_from_slice(&0x7000u64.to_le_bytes());
        addr[24..32].copy_from_slice(&512u64.to_le_bytes());
        addr[32..36].copy_from_slice(&109u32.to_le_bytes());
        let mut info = [0u8; 32];
        info[16..20].copy_from_slice(&UNIFORM_TEXEL_BUFFER.to_le_bytes());
        info[24..32].copy_from_slice(&(addr.as_ptr() as u64).to_le_bytes());
        assert_eq!(unsafe { texel_input_of(info.as_ptr()) }, Some((0x7000, 512, 109)));
        info[16..20].copy_from_slice(&UNIFORM_BUFFER.to_le_bytes());
        assert_eq!(unsafe { texel_input_of(info.as_ptr()) }, None);
    }

}
