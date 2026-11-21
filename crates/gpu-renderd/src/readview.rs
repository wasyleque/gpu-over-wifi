//! Wspólny widok pamięci sesji dla połączenia odczytów (`AttachReads` / `MemReadAfter`): wątek odczytu czyta odwzorowaną pamięć GPU, nie czekając na całą
//! kolejkę poleceń sesji, tylko na ramkę, od której zależy (zwykle ostatnie oczekiwanie na GPU). Wykonawca (wątek poleceń) zgłasza postęp numerem ramki
//! (`advance`), a odwzorowania pamięci wpisuje i usuwa w tabeli pod blokadą zapisu: wątek odczytu trzyma blokadę odczytu przez cały czas kopiowania,
//! więc `vkUnmapMemory`/`vkFreeMemory` nigdy nie wyprzedzi kopii w toku.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Odwzorowanie pamięci urządzenia: wskaźnik hosta, rozmiar i uchwyty potrzebne do `vkInvalidateMappedMemoryRanges`.
#[derive(Clone, Copy, Debug)]
pub struct MapInfo {
    pub ptr: usize,
    pub size: u64,
    pub device: u64,
    pub memory: u64,
}

pub struct ReadView {
    /// Numer (16 bitów, jak `Frame::seq`) ostatniej wykonanej ramki strumienia sterującego; `has_seq` = była już jakaś.
    processed: AtomicU64,
    has_seq: AtomicBool,
    mu: Mutex<()>,
    cv: Condvar,
    waiters: AtomicUsize,
    table: RwLock<HashMap<u64, MapInfo>>,
    /// Wskaźnik na `vkInvalidateMappedMemoryRanges` (0 = jeszcze nieznany: odczyt zwraca błąd i klient wraca do strumienia sterującego).
    invalidate: AtomicUsize,
}

/// Ile ramek wstecz uznajemy za "już wykonane" (reszta okna 16 bitów to "jeszcze nie": ramka, na którą czekamy, jest co najwyżej `65536 - PAST_WINDOW` = 20536 ramek
/// przed wykonawcą, a to z zapasem więcej niż kolejka w locie). Poprzednie symetryczne okno ±32768 uznawało zapis sprzed 33-65 tys. ramek za "z przyszłości" i serwer
/// czekał na niego do limitu (Rise of the Tomb Raider wysyła ok. 150 tys. ramek pamięci na przebieg).
const PAST_WINDOW: u16 = 45000;

/// `after` jest już wykonane, gdy `processed` nie jest przed nim (porównanie z zawijaniem 16 bitów, okno asymetryczne: patrz `PAST_WINDOW`).
pub fn reached(processed: u16, after: u16) -> bool {
    processed.wrapping_sub(after) <= PAST_WINDOW
}

impl ReadView {
    pub fn new() -> Arc<ReadView> {
        Arc::new(ReadView {
            processed: AtomicU64::new(0),
            has_seq: AtomicBool::new(false),
            mu: Mutex::new(()),
            cv: Condvar::new(),
            waiters: AtomicUsize::new(0),
            table: RwLock::new(HashMap::new()),
            invalidate: AtomicUsize::new(0),
        })
    }

    /// Wykonawca skończył ramkę `seq` (po powrocie z obsługi, także blokującego oczekiwania).
    pub fn advance(&self, seq: u16) {
        self.processed.store(seq as u64, Ordering::SeqCst);
        self.has_seq.store(true, Ordering::SeqCst);
        if self.waiters.load(Ordering::SeqCst) > 0 {
            let _g = self.mu.lock().unwrap_or_else(|e| e.into_inner());
            self.cv.notify_all();
        }
    }

    /// Czeka, aż wykonawca skończy ramkę `after` (albo późniejszą); `false` po przekroczeniu czasu.
    pub fn wait_for(&self, after: u16, timeout: Duration) -> bool {
        let done = || self.has_seq.load(Ordering::SeqCst) && reached(self.processed.load(Ordering::SeqCst) as u16, after);
        if done() {
            return true;
        }
        let deadline = Instant::now() + timeout;
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let mut g = self.mu.lock().unwrap_or_else(|e| e.into_inner());
        let ok = loop {
            if done() {
                break true;
            }
            let now = Instant::now();
            if now >= deadline {
                break false;
            }
            g = self.cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        };
        drop(g);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        ok
    }

    pub fn set_invalidate(&self, f: *const c_void) {
        self.invalidate.store(f as usize, Ordering::SeqCst);
    }

    pub fn insert(&self, memory: u64, info: MapInfo) {
        self.table.write().unwrap_or_else(|e| e.into_inner()).insert(memory, info);
    }

    /// Usuwa wpis; wraca dopiero, gdy żaden odczyt w toku nie używa już odwzorowania (blokada zapisu czeka na blokady odczytu).
    pub fn remove(&self, memory: u64) {
        self.table.write().unwrap_or_else(|e| e.into_inner()).remove(&memory);
    }

    pub fn clear(&self) {
        self.table.write().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Kopiuje `len` bajtów z `memory` od `offset` (po `vkInvalidateMappedMemoryRanges`). `Err` = nieznana pamięć/zakres albo brak funkcji.
    pub fn read(&self, memory: u64, offset: u64, len: u32) -> Result<Vec<u8>, i32> {
        let inv = self.invalidate.load(Ordering::SeqCst);
        if inv == 0 {
            return Err(-1);
        }
        let g = self.table.read().unwrap_or_else(|e| e.into_inner());
        let m = g.get(&memory).copied().ok_or(-1)?;
        let end = offset.checked_add(len as u64).ok_or(-1)?;
        if end > m.size {
            return Err(-1);
        }
        let mut r = [0u64; 5];
        r[0] = 6; // VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE
        r[2] = m.memory;
        r[4] = u64::MAX; // VK_WHOLE_SIZE
        // SAFETY: `inv` to wskaźnik na vkInvalidateMappedMemoryRanges (device, count, pRanges); struktura żyje na stosie; blokada odczytu tabeli
        // trzyma odwzorowanie (usunięcie czeka na jej zwolnienie), więc `ptr` jest ważny przez całe kopiowanie.
        unsafe {
            let res = crate::vk::replay::call_thunk(inv as *const c_void, &[m.device, 1, r.as_ptr() as u64], &[]).ok_or(-1)?;
            if (res as u32 as i32) < 0 {
                return Err(res as u32 as i32);
            }
            Ok(std::slice::from_raw_parts((m.ptr as *const u8).add(offset as usize), len as usize).to_vec())
        }
    }
}

static HUB: Mutex<Option<HashMap<u64, Arc<ReadView>>>> = Mutex::new(None);

fn with_map<R>(f: impl FnOnce(&mut HashMap<u64, Arc<ReadView>>) -> R) -> R {
    let mut g = HUB.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// Rejestruje widok pod identyfikatorem sesji (może być kilka identyfikatorów jednego widoku: `SessionId` tworzy nowy po zmianie swapchaina).
pub fn register(id: u64, view: &Arc<ReadView>) {
    with_map(|m| m.insert(id, view.clone()));
}

pub fn find(id: u64) -> Option<Arc<ReadView>> {
    with_map(|m| m.get(&id).cloned())
}

pub fn forget(id: u64) {
    with_map(|m| m.remove(&id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static INVALIDATIONS: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn fake_invalidate(_device: u64, count: u64, ranges: u64) -> u64 {
        assert_eq!(count, 1);
        assert_ne!(ranges, 0);
        INVALIDATIONS.fetch_add(1, Ordering::SeqCst);
        0
    }

    #[test]
    fn reached_handles_u16_wraparound() {
        assert!(reached(10, 10));
        assert!(reached(11, 10));
        assert!(!reached(9, 10));
        assert!(reached(3, 65530), "po zawinięciu 3 jest za 65530");
        assert!(!reached(65530, 3), "65530 jest przed 3 po zawinięciu");
        assert!(reached(100, 100u16.wrapping_sub(40000)), "zapis sprzed 40 tys. ramek jest już wykonany (dawne okno ±32768 czekało na niego)");
        assert!(!reached(100, 100 + 5000), "zapis 5 tys. ramek w przód to jeszcze nie");
    }

    #[test]
    fn wait_for_blocks_until_the_executor_reaches_the_frame_and_times_out_otherwise() {
        let v = ReadView::new();
        assert!(!v.wait_for(5, Duration::from_millis(20)), "żadna ramka jeszcze nie wykonana");
        let v2 = v.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            v2.advance(4);
            std::thread::sleep(Duration::from_millis(30));
            v2.advance(5);
        });
        let t0 = Instant::now();
        assert!(v.wait_for(5, Duration::from_secs(5)));
        assert!(t0.elapsed() >= Duration::from_millis(50), "czekanie musi trwać do ramki 5, nie 4");
        assert!(v.wait_for(3, Duration::from_millis(1)), "ramka 3 jest już za nami");
        t.join().unwrap();
    }

    #[test]
    fn read_copies_after_invalidate_and_rejects_unknown_memory_and_ranges() {
        let v = ReadView::new();
        let data: Vec<u8> = (0..=255u8).collect();
        assert!(v.read(1, 0, 4).is_err(), "brak funkcji invalidate");
        v.set_invalidate(fake_invalidate as *const () as *const c_void);
        assert!(v.read(1, 0, 4).is_err(), "pamięć nieznana");
        v.insert(1, MapInfo { ptr: data.as_ptr() as usize, size: 256, device: 7, memory: 9 });
        let before = INVALIDATIONS.load(Ordering::SeqCst);
        assert_eq!(v.read(1, 10, 3).unwrap(), vec![10, 11, 12]);
        assert_eq!(INVALIDATIONS.load(Ordering::SeqCst), before + 1);
        assert!(v.read(1, 250, 7).is_err(), "poza rozmiarem");
        assert!(v.read(1, u64::MAX, 2).is_err(), "przepełnienie");
        v.remove(1);
        assert!(v.read(1, 0, 4).is_err(), "po usunięciu");
    }

    #[test]
    fn remove_waits_for_a_read_in_progress() {
        // Odczyt trzyma blokadę odczytu tabeli w trakcie kopiowania: `remove` (blokada zapisu) nie może wyprzedzić; tu sprawdzamy tylko, że oba działają współbieżnie bez zakleszczenia.
        let v = ReadView::new();
        let data = vec![7u8; 1 << 20];
        v.set_invalidate(fake_invalidate as *const () as *const c_void);
        v.insert(2, MapInfo { ptr: data.as_ptr() as usize, size: data.len() as u64, device: 1, memory: 1 });
        let mut hs = Vec::new();
        for _ in 0..4 {
            let v = v.clone();
            hs.push(std::thread::spawn(move || {
                for _ in 0..200 {
                    let _ = v.read(2, 0, 1 << 16);
                }
            }));
        }
        for _ in 0..50 {
            v.remove(2);
            v.insert(2, MapInfo { ptr: data.as_ptr() as usize, size: data.len() as u64, device: 1, memory: 1 });
        }
        for h in hs {
            h.join().unwrap();
        }
    }
}
