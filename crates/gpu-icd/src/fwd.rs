//! Generyczne przekazywanie poleceń Vulkana do serwera.
//!
//! `entry::<OP>` to odwrotność thunka serwera: jeden odbiornik czyta argumenty całkowite z rejestrów i stosu oraz
//! `float`/`double` z `xmm` (System V x86_64), więc obsługuje dowolne polecenie bez ręcznej sygnatury. Dalej:
//! parametry trafiają do 8-bajtowych slotów, kodek zamienia je na bajty, a klasa polecenia (z `vk.xml`) wybiera
//! drogę: partia bez czekania (większość) albo synchroniczne zapytanie (polecenia z wynikiem lub oczekiwaniem).

use std::collections::HashMap;

use gpu_proto::meta::{CmdClass, Elem, Ret, Shape, COMMANDS};
use gpu_proto::msg::{Request, Response, MAX_BATCH};
use gpu_vkcodec::codec::{decode_out_params, encode_command, out_handles, Ctx};
use gpu_vkcodec::CodecError;
use gpu_wire::{Decoder, Encoder};

use super::{lock, InstState, InstanceObj, CURRENT};

pub type Entry = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, f64, f64, f64, f64, f64, f64, f64, f64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;

const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_MEMORY_MAP_FAILED: i32 = -5;
const VK_ERROR_DEVICE_LOST: i32 = -4;
/// Partia wychodzi, gdy przekroczy ten rozmiar (albo na punkcie synchronizacji).
const BATCH_FLUSH: usize = 64 * 1024;
/// Największy deskryptor, który zapamiętujemy (sterowniki zwracają kilkadziesiąt bajtów) i limit liczby wpisów cache.
const DESC_CACHE_MAX_BYTES: u64 = 1024;
const DESC_CACHE_MAX_ENTRIES: usize = 200_000;
const MEM_CHUNK: usize = 4 * 1024 * 1024;
/// Najmniejszy zapis, dla którego warto próbować LZ4 (mniejsze idą surowo: narzut przewyższa zysk).
const LZ4_MIN_WRITE: usize = 4096;

const DISPATCHABLE: &[&str] = &["VkInstance", "VkPhysicalDevice", "VkDevice", "VkQueue", "VkCommandBuffer"];

/// Obiekt dyspozycyjny ICD (urządzenie, kolejka, bufor poleceń): wskaźnik loadera i identyfikator protokołu.
#[repr(C)]
pub struct DispObj {
    loader_data: usize,
    id: u64,
    /// Dla bufora poleceń: zakodowane `vkCmd*` jeszcze niewysłane do wspólnej partii (szybka ścieżka, patrz `fast_record`). Bufor jest
    /// PER BUFOR POLECEŃ, a nie per wątek: vkd3d-proton nagrywa jeden bufor na kilku wątkach, a polecenia z wątku A nie mogą wyprzedzić
    /// `vkEndCommandBuffer` wywołanego na wątku B (serwer dostawał `vkCmd*` po zamknięciu bufora i sterownik NVIDIA kończył się segfaultem).
    pending: std::sync::Mutex<Vec<u8>>,
}

impl DispObj {
    pub(crate) fn new(id: u64) -> DispObj {
        DispObj { loader_data: 0, id, pending: std::sync::Mutex::new(Vec::new()) }
    }

    /// Dopisuje zakodowane polecenie; zwraca, czy bufor osiągnął próg opróżnienia.
    fn push(&self, bytes: &[u8]) -> bool {
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.extend_from_slice(bytes);
        p.len() >= LOCAL_FLUSH_BYTES
    }

    /// Wydaje zebrane polecenia (kolejność zachowana) i czyści bufor.
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Liczniki ruchu do pomiaru bazowego (M2): ile bajtów i obiegów kosztuje naiwne przekazywanie.
#[derive(Default, Debug)]
pub struct Stats {
    pub cmd_bytes: u64,
    pub cmd_batches: u64,
    pub memw_bytes: u64,
    /// Bajty zapisów pamięci faktycznie wysłane łączem (po kompresji LZ4).
    pub memw_wire_bytes: u64,
    /// Odświeżenia pamięci według planu: całe / tylko zakresy (i ile bajtów obejmowały zakresy łącznie).
    pub refresh_whole: u64,
    pub refresh_ranged: u64,
    pub refresh_ranged_bytes: u64,
    /// `vkGetDescriptorEXT`: odpowiedzi z cache / z serwera.
    pub desc_synth: u64,
    /// Skanowanie pamięci przy submit: przejścia przez jądrowe śledzenie (liczba, ns) i pełne haszowanie bez śledzenia (liczba, ns, bajty).
    pub scan_tracked_n: u64,
    pub scan_tracked_ns: u64,
    pub scan_hashed_n: u64,
    pub scan_hashed_ns: u64,
    pub scan_hashed_bytes: u64,
    pub mapped_count_max: u64,
    pub desc_hits: u64,
    pub desc_misses: u64,
    /// Chybienia cache deskryptorów według `VkDescriptorType` (0..=10; ostatnia pozycja: inne).
    pub desc_miss_by_type: [u64; 12],
    pub lazy_memories: u64,
    /// Pamięć -> (rozmiar, flagi właściwości) oraz wysyłki zapisów według pamięci: (liczba, bajty surowo, bajty na łączu). Do logu.
    pub mem_flags: std::collections::HashMap<u64, (u64, u32)>,
    pub send_by_mem: std::collections::HashMap<u64, (u64, u64, u64)>,
    pub lazy_invalidations: u64,
    /// Obiegi po sieci według polecenia: (liczba, łączny czas oczekiwania w ns). Do rankingu, co jeszcze blokuje klatki.
    pub rt_by_cmd: std::collections::HashMap<&'static str, (u64, u64)>,
    pub memr_bytes: u64,
    pub query_bytes: u64,
    pub roundtrips: u64,
    pub commands: u64,
    /// Czasy etapów `Present` (ns): submit+czekanie na GPU serwera, odczyt klatki po sieci, lokalne wyświetlenie; liczba klatek.
    pub t_wait_ns: u64,
    pub t_read_ns: u64,
    pub t_local_ns: u64,
    pub frames: u64,
    /// Skanowanie odwzorowanej pamięci przed `vkQueueSubmit`: czas (ns), przeskanowane bajty, wysłane bajty różnic i czas wysyłki.
    pub t_scan_ns: u64,
    pub scan_bytes: u64,
    pub t_memsend_ns: u64,
    /// Odświeżanie odwzorowanej pamięci po oczekiwaniu (czas, liczba zapytań) oraz czas samej odpowiedzi serwera na polecenia synchroniczne.
    pub t_refresh_ns: u64,
    pub refreshes: u64,
    pub t_syncrecv_ns: u64,
    /// Diagnostyka zapisów pamięci (tylko przy `GPU_ICD_DEBUG`): strony 4 KiB nowe i powtórzone w sesji oraz wynik próbnej kompresji LZ4.
    pub pages_new: u64,
    pub pages_dup: u64,
    pub lz4_in: u64,
    pub lz4_out: u64,
    pub lz4_samples: u64,
}

impl Stats {
    /// Ranking pamięci według bajtów wysłanych na serwer: `id (rozmiar MB, flagi) wysyłek, surowo MB, łącze MB`.
    fn top_uploads(&self) -> String {
        let mut v: Vec<_> = self.send_by_mem.iter().collect();
        v.sort_by_key(|(_, (_, raw, _))| std::cmp::Reverse(*raw));
        v.iter()
            .take(6)
            .map(|(id, (n, raw, wire))| {
                let (size, fl) = self.mem_flags.get(id).copied().unwrap_or((0, 0));
                format!("{:#x} ({} MB, flagi {:#x}) {}x {} MB surowo / {} MB łącze", id, size >> 20, fl, n, raw >> 20, wire >> 20)
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Ranking poleceń według łącznego czasu obiegów: `nazwa liczba ms (średnio µs)`.
    fn top_roundtrips(&self) -> String {
        let mut v: Vec<_> = self.rt_by_cmd.iter().collect();
        v.sort_by_key(|(_, (_, ns))| std::cmp::Reverse(*ns));
        v.iter().take(14).map(|(n, (c, ns))| format!("{} {}x {:.0}ms ({:.0}µs)", n.trim_start_matches("vk"), c, *ns as f64 / 1e6, *ns as f64 / 1e3 / (*c).max(1) as f64)).collect::<Vec<_>>().join("; ")
    }

    pub fn report(&self) -> String {
        let per = |v: u64| if self.frames == 0 { 0.0 } else { v as f64 / self.frames as f64 / 1e6 };
        format!(
            "gpu-icd statystyki: strony pamięci nowe/powtórzone {}/{}, LZ4 próbka {}→{} B; skan: śledzone {} razy ({:.0} ms), haszowane {} razy ({:.0} ms, {} MB), pamięci odwzorowanych maks. {}; deskryptory (lokalnie/cache/serwer) {}/{}/{}, chybienia wg typu {:?}; odświeżenia pamięci {} ({:.0} ms; całe {}, zakresy {} po średnio {} KB), oczekiwanie na odpowiedzi serwera {:.0} ms, skan pamięci {:.0} ms ({} MB), wysyłka różnic {:.0} ms; poleceń {}, partii {}, bajty poleceń {}, zapisy pamięci {} B (na łączu {} B), odczyty pamięci {} B, zapytania {} B, obiegi po sieci {}; klatek {}: na klatkę czekanie na GPU {:.1} ms, odczyt {:.1} ms, wyświetlenie lokalne {:.1} ms",
            self.pages_new, self.pages_dup, self.lz4_in, self.lz4_out, self.scan_tracked_n, self.scan_tracked_ns as f64 / 1e6, self.scan_hashed_n, self.scan_hashed_ns as f64 / 1e6, self.scan_hashed_bytes / 1_000_000, self.mapped_count_max, self.desc_synth, self.desc_hits, self.desc_misses, self.desc_miss_by_type, self.refreshes, self.t_refresh_ns as f64 / 1e6, self.refresh_whole, self.refresh_ranged, self.refresh_ranged_bytes / self.refresh_ranged.max(1) / 1024, self.t_syncrecv_ns as f64 / 1e6, self.t_scan_ns as f64 / 1e6, self.scan_bytes / 1_000_000, self.t_memsend_ns as f64 / 1e6, self.commands, self.cmd_batches, self.cmd_bytes, self.memw_bytes, self.memw_wire_bytes, self.memr_bytes, self.query_bytes, self.roundtrips, self.frames, per(self.t_wait_ns), per(self.t_read_ns), per(self.t_local_ns)
        )
    }
}

/// Flagi diagnostyczne czytane RAZ (zmienne środowiskowe nie zmieniają się w trakcie procesu, a odczyt przy każdym poleceniu
/// kosztował zauważalnie przy setkach tysięcy poleceń na sekundę).
pub struct Flags {
    pub debug: bool,
    pub trace: bool,
    pub periodic: bool,
    /// Doświadczalnie: nie odświeżaj z serwera pamięci, które nie są HOST_CACHED (`GPU_ICD_NO_REFRESH_UNCACHED=1`).
    pub no_refresh_uncached: bool,
    /// Skan odwzorowanej pamięci przy każdym `vkQueueSubmit` (dawne zachowanie) zamiast odroczonego do wysłania partii.
    pub eager_scan: bool,
    /// Odczyt leniwy pamięci HOST_CACHED (domyślnie włączony; `GPU_ICD_NO_LAZY=1` wyłącza).
    pub lazy: bool,
    /// Spekulatywne odpowiedzi `vkAllocateDescriptorSets` z księgowości puli po stronie klienta (domyślnie włączone; `GPU_ICD_NO_DESC_SPEC=1`
    /// wyłącza). ATS robi ich 1,8 mln na przebieg po 125 µs obiegu (ok. 18 ms z klatki 22 ms), więc to największy koszt. Bezpieczeństwo: zapas w puli
    /// (`DESC_SPEC_MARGIN`) i audyt: co `DESC_SPEC_AUDIT_EVERY`-ta alokacja idzie synchronicznie; odmowa serwera tam, gdzie przewidzieliśmy
    /// sukces, wyłącza spekulację na stałe.
    pub desc_spec: bool,
    /// Wyłączniki do szukania przyczyn zawieszeń: `GPU_ICD_LOCKED_WAIT=1` (czekanie na odpowiedź z blokadą instancji, jak dawniej).
    pub locked_wait: bool,
    /// Odcinkowe oczekiwania na semafory i fence'e (`GPU_ICD_SLICE_WAIT=1`): doświadczalne, domyślnie wyłączone do czasu testu w grze.
    pub slice_wait: bool,
    /// Szybka ścieżka poleceń `vkCmd*` bez blokady instancji (domyślnie włączona; `GPU_ICD_NO_FASTPATH=1` wyłącza). Dawny crash sterownika
    /// NVIDIA na serwerze miał przyczynę w buforach per wątek zamiast per bufor poleceń; od naprawy pełne sesje Cyberpunka przechodzą.
    pub fastpath: bool,
}

pub fn flags() -> &'static Flags {
    static F: std::sync::OnceLock<Flags> = std::sync::OnceLock::new();
    F.get_or_init(|| Flags {
        no_refresh_uncached: std::env::var_os("GPU_ICD_NO_REFRESH_UNCACHED").is_some(),
        eager_scan: std::env::var_os("GPU_ICD_EAGER_SCAN").is_some(),
        lazy: std::env::var_os("GPU_ICD_NO_LAZY").is_none(),
        desc_spec: std::env::var_os("GPU_ICD_NO_DESC_SPEC").is_none(),
        locked_wait: std::env::var_os("GPU_ICD_LOCKED_WAIT").is_some(),
        slice_wait: std::env::var_os("GPU_ICD_SLICE_WAIT").is_some(),
        fastpath: std::env::var_os("GPU_ICD_NO_FASTPATH").is_none(),
        debug: std::env::var_os("GPU_ICD_DEBUG").is_some(),
        trace: std::env::var_os("GPU_ICD_TRACE").is_some(),
        // Cykliczne liczniki co 5 s: `GPU_ICD_STATS=periodic` albo (jak dotąd) razem z `GPU_ICD_DEBUG`.
        periodic: std::env::var("GPU_ICD_STATS").is_ok_and(|v| v == "periodic") || std::env::var_os("GPU_ICD_DEBUG").is_some(),
    })
}

/// Lokalna kopia odwzorowanej pamięci (M2a: całe obiekty, M3: tylko zmienione strony).
pub struct Shadow {
    data: crate::memtrack::PageAlignedBuf,
    mapped: bool,
    /// Stan stron znany serwerowi: wysyłamy i odbieramy tylko różnice.
    pages: gpu_proto::pagediff::PageTracker,
    /// Jądrowe śledzenie zapisów aplikacji (duże pamięci): zamiast skrótów całej pamięci sprawdzamy tylko zapisane strony.
    tracker: Option<crate::memtrack::Tracker>,
    /// Odczyt leniwy (`lazymap`): po oczekiwaniu na GPU strony są tylko unieważniane, a pobierane przy dotknięciu przez aplikację.
    lazy: Option<std::sync::Arc<crate::lazymap::LazyRegion>>,
    /// Wersja (bilet odpowiedzi + 1) danych serwera zapisanych na każdej stronie 4 KiB: odpowiedź o niższym bilecie nie nadpisze nowszej.
    /// Przy zwalnianiu blokady na czas oczekiwania odpowiedzi trafiają do pamięci w dowolnej kolejności, a serwer liczy różnice względem
    /// poprzedniej wysłanej: starsza różnica zastosowana po nowszej cofnęłaby strony do nieaktualnej treści (np. wartości fence'a).
    ver: Vec<u64>,
}

/// Zapas puli (zestawy i deskryptory każdego typu), poniżej którego alokacje zestawów idą synchronicznie, oraz częstotliwość audytu.
pub const DESC_SPEC_MARGIN: u32 = 64;
/// Audyt co 1024. alokację (przy 900 alokacjach na klatkę w ATS audyt co 64. kosztował 17 ms z klatki 166 ms), a na początku gęściej (co 8. z pierwszych 256).
const DESC_SPEC_AUDIT_EVERY: u64 = 1024;
static DESC_SPEC_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DESC_SPEC_BROKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Zakres rozmiarów pamięci z odczytem leniwym (GravityMark: 8 MB pamięć bez cache odświeżana po każdym oczekiwaniu = 21 ms na żądanie): mniejsze są tanie do odświeżenia, większe zajęłyby za dużo pamięci na kopię.
fn lazy_all() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("GPU_ICD_LAZY_ALL").is_some())
}

const LAZY_MIN: usize = 64 * 1024;
const LAZY_MAX: usize = 256 * 1024 * 1024;

/// Od tego rozmiaru pamięć ma jądrowe śledzenie zapisów (mniejsze są tanie do przeskanowania w całości).
const TRACK_FROM: usize = 1024 * 1024;

impl Shadow {
    pub fn new(len: usize) -> Shadow {
        // Preferowana jest wspólna arena (jedno wywołanie skanu dla wszystkich pamięci, także małych); przy jej braku osobny bufor z własnym
        // śledzeniem (duże pamięci) albo pełnym skanem skrótów (małe).
        if let Some(data) = crate::memtrack::PageAlignedBuf::new_in_arena(len) {
            return Shadow { ver: vec![0; len.div_ceil(4096)], data, mapped: false, pages: gpu_proto::pagediff::PageTracker::new(len), tracker: None, lazy: None };
        }
        let data = crate::memtrack::PageAlignedBuf::new(len);
        let tracker = if len >= TRACK_FROM && std::env::var("GPU_ICD_NO_MEMTRACK").is_err() { crate::memtrack::Tracker::new(data.as_ptr(), data.capacity()) } else { None };
        Shadow { ver: vec![0; len.div_ceil(4096)], data, mapped: false, pages: gpu_proto::pagediff::PageTracker::new(len), tracker, lazy: None }
    }

    /// Pamięć z odczytem leniwym (podwójne odwzorowanie `memfd` zarejestrowane w `userfaultfd`); `None`, gdy jądro tego nie obsługuje.
    pub fn new_lazy(len: usize, memory: u64) -> Option<Shadow> {
        let data = crate::memtrack::PageAlignedBuf::new_shared(len)?;
        let region = crate::lazymap::LazyRegion::register(memory, data.app_ptr(), data.as_ptr(), data.capacity())?;
        let Some(tracker) = crate::memtrack::Tracker::attached(data.app_ptr(), data.capacity()) else {
            region.unregister();
            return None;
        };
        Some(Shadow { ver: vec![0; len.div_ceil(4096)], data, mapped: false, pages: gpu_proto::pagediff::PageTracker::new(len), tracker: Some(tracker), lazy: Some(region) })
    }

    /// Wskaźnik dla aplikacji (`vkMapMemory`): widok objęty obsługą błędów stron albo zwykły bufor.
    pub fn app_ptr(&self) -> *mut u8 {
        self.data.app_ptr()
    }

    /// Zakresy zmienione przez aplikację od ostatniego wysłania. Z jądrowym śledzeniem sprawdzamy tylko zapisane strony
    /// (potwierdzone skrótem, więc strony zapisane bez zmiany treści nie idą do sieci); bez niego skanujemy całość.
    pub fn dirty_ranges(&mut self) -> Vec<(usize, usize)> {
        self.dirty_ranges_timed().0
    }

    /// Jak `dirty_ranges`, ale zwraca też, czy użyto jądrowego śledzenia (`true`) i ile bajtów zahaszowano w pełnym skanie (0 przy śledzeniu).
    pub fn dirty_ranges_timed(&mut self) -> (Vec<(usize, usize)>, bool, usize) {
        if let (Some(off), Some(a)) = (self.data.arena_offset(), crate::memtrack::arena()) {
            if let Some(cand) = a.take_dirty_range(off, self.data.capacity()) {
                return (self.pages.changed_within(&self.data, &cand), true, 0);
            }
        }
        if let Some(t) = &self.tracker {
            match t.take_dirty(0, self.data.capacity()) {
                Some(mut cand) => {
                    // Pamięć z odczytem leniwym: strony zrzucone przez `invalidate` straciły flagę "zapisane", więc zawsze je weryfikujemy skrótami.
                    if let Some(r) = &self.lazy {
                        cand.extend(r.take_recheck());
                    }
                    return (self.pages.changed_within(&self.data, &cand), true, 0);
                }
                None => self.tracker = None, // śledzenie przestało działać: wracamy do pełnego skanowania
            }
        }
        let n = self.data.len();
        (self.pages.changed(&self.data, usize::MAX).0, false, n)
    }

    /// Wpisuje dane serwera `body` pod `a`, ale tylko na strony, na których nie ma jeszcze nowszej odpowiedzi (`ticket` to bilet tej).
    pub fn apply_at(&mut self, a: usize, body: &[u8], ticket: u64) {
        const P: usize = 4096;
        let end = a + body.len();
        if end > self.data.len() || body.is_empty() {
            return;
        }
        let mut p = a / P;
        while p * P < end {
            let (ps, pe) = ((p * P).max(a), ((p + 1) * P).min(end));
            if self.ver.get(p).is_some_and(|v| *v <= ticket) {
                self.data[ps..pe].copy_from_slice(&body[ps - a..pe - a]);
                self.ver[p] = ticket + 1;
            }
            p += 1;
        }
        self.mark_synced(a, end);
    }

    /// Zmienione zakresy: ze zbiorczego skanu areny (jeśli pamięć w niej leży i skan się udał), w przeciwnym razie skan pojedynczy.
    pub fn changed_from_bulk_or_scan(&mut self, bulk: Option<&[(usize, usize)]>) -> (Vec<(usize, usize)>, bool, usize) {
        if let Some(b) = bulk {
            if let Some(r) = self.changed_from_bulk(b) {
                return (r, true, 0);
            }
        }
        self.dirty_ranges_timed()
    }

    /// Zmienione zakresy tej pamięci na podstawie zbiorczego skanu areny (`bulk`: posortowane zakresy `[od, do)` w offsetach areny); `None`,
    /// gdy pamięć nie leży w arenie. Strony zapisane bez zmiany treści odsiewa porównanie skrótów, jak przy skanie pojedynczym.
    pub fn changed_from_bulk(&mut self, bulk: &[(usize, usize)]) -> Option<Vec<(usize, usize)>> {
        let off = self.data.arena_offset()?;
        let end = off + self.data.capacity();
        let first = bulk.partition_point(|r| r.1 <= off);
        let cand: Vec<(usize, usize)> = bulk[first..].iter().take_while(|r| r.0 < end).map(|r| (r.0.max(off) - off, r.1.min(end) - off)).collect();
        if cand.is_empty() {
            return Some(Vec::new());
        }
        Some(self.pages.changed_within(&self.data, &cand))
    }

    pub fn mark_synced(&mut self, from: usize, to: usize) {
        self.pages.mark_synced(&self.data, from, to);
    }
}

impl Drop for Shadow {
    fn drop(&mut self) {
        if let Some(r) = &self.lazy {
            r.unregister();
        }
    }
}

/// Tłumaczy uchwyty dyspozycyjne (wskaźnik na obiekt -> identyfikator pod offsetem 8); reszta to identyfikatory.
struct IcdCtx;

impl Ctx for IcdCtx {
    fn handle_to_wire(&self, ty: &str, raw: u64) -> Result<u64, CodecError> {
        if raw != 0 && DISPATCHABLE.contains(&ty) {
            // SAFETY: uchwyty dyspozycyjne to wskaźniki na obiekty ICD z identyfikatorem pod offsetem 8.
            Ok(unsafe { std::ptr::read_unaligned((raw as *const u8).add(8) as *const u64) })
        } else {
            Ok(raw)
        }
    }
    fn wire_to_handle(&self, _ty: &str, wire: u64) -> Result<u64, CodecError> {
        Err(CodecError::Handle(wire))
    }
}

impl InstState {
    pub(crate) fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn lose(&mut self) {
        self.device_lost = true;
    }

    /// Wysyła zgromadzoną partię poleceń (bez czekania na odpowiedź).
    pub(crate) fn flush(&mut self) {
        if self.scan_pending {
            self.settle_memory();
        }
        if self.batch.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.batch);
        self.stats.cmd_bytes += data.len() as u64;
        self.stats.cmd_batches += 1;
        if self.client.send(&Request::Cmds { data }).is_err() {
            self.lose();
        }
    }

    fn send_mem(&mut self, memory: u64, offset: u64, data: &[u8]) {
        self.flush();
        if flags().debug {
            self.survey_write(data);
        }
        self.stats.memw_bytes += data.len() as u64;
        let wire_before = self.stats.memw_wire_bytes;
        let acct = self.stats.send_by_mem.entry(memory).or_insert((0, 0, 0));
        acct.0 += 1;
        acct.1 += data.len() as u64;
        for (i, chunk) in data.chunks(MEM_CHUNK.min(MAX_BATCH)).enumerate() {
            let at = offset + (i * MEM_CHUNK) as u64;
            // Strony małe albo źle kompresowalne (np. tekstury BCn) idą surowo: LZ4 wysyłamy tylko, gdy oszczędza co najmniej 10%.
            let packed = (chunk.len() >= LZ4_MIN_WRITE).then(|| gpu_proto::video::compress_lz4(chunk)).filter(|c| c.len() * 10 <= chunk.len() * 9);
            let req = match packed {
                Some(c) => {
                    self.stats.memw_wire_bytes += c.len() as u64;
                    Request::MemWriteLz4 { memory, offset: at, raw_len: chunk.len() as u32, data: c }
                }
                None => {
                    self.stats.memw_wire_bytes += chunk.len() as u64;
                    Request::MemWrite { memory, offset: at, data: chunk.to_vec() }
                }
            };
            if self.client.send(&req).is_err() {
                self.lose();
            }
        }
        let wire = self.stats.memw_wire_bytes - wire_before;
        if let Some(a) = self.stats.send_by_mem.get_mut(&memory) {
            a.2 += wire;
        }
    }

    /// Diagnostyka (tylko przy `GPU_ICD_DEBUG`): ile stron 4 KiB wysyłanych w tej sesji już wcześniej widzieliśmy (potencjał cache
    /// zawartości po stronie serwera) i jak dobrze kompresuje się próbka danych (potencjał LZ4).
    fn survey_write(&mut self, data: &[u8]) {
        for page in data.chunks(4096) {
            let h = gpu_proto::pagediff::page_hash(page);
            if self.seen_pages.len() < 8_000_000 {
                if self.seen_pages.insert(h) {
                    self.stats.pages_new += 1;
                } else {
                    self.stats.pages_dup += 1;
                }
            }
        }
        // Próbka: co 16. zapis do 256 KiB.
        self.stats.lz4_samples += 1;
        if self.stats.lz4_samples % 16 == 1 {
            let sample = &data[..data.len().min(256 * 1024)];
            self.stats.lz4_in += sample.len() as u64;
            self.stats.lz4_out += gpu_proto::video::compress_lz4(sample).len() as u64;
        }
    }

    /// Odroczone skanowanie odwzorowanej pamięci (patrz `scan_pending`): zapisy pamięci idą na serwer PRZED partią poleceń, która zawiera
    /// zgłoszenia (`vkQueueSubmit`), więc serwer wykonuje je na aktualnej zawartości. Partia jest odkładana na czas skanu, bo `send_mem`
    /// wysyła zaległe polecenia przed każdym zapisem pamięci.
    pub(crate) fn settle_memory(&mut self) {
        self.scan_pending = false;
        let held = std::mem::take(&mut self.batch);
        self.write_mapped();
        let mut tail = std::mem::replace(&mut self.batch, held);
        self.batch.append(&mut tail);
    }

    /// Przed `vkQueueSubmit`: serwer musi zobaczyć wszystko, co aplikacja wpisała w odwzorowaną pamięć.
    fn write_mapped(&mut self) {
        let ids: Vec<u64> = self.mems.iter().filter(|(_, s)| s.mapped).map(|(i, _)| *i).collect();
        self.stats.mapped_count_max = self.stats.mapped_count_max.max(ids.len() as u64);
        // Zapisy aplikacji do wszystkich pamięci w arenie dostajemy jednym skanem; pamięci bez zapisów w ogóle nie są dotykane.
        let t_bulk = std::time::Instant::now();
        let bulk: Option<Vec<(usize, usize)>> = if ids.iter().any(|i| self.mems.get(i).is_some_and(|s| s.data.arena_offset().is_some())) { crate::memtrack::arena().and_then(|a| a.take_dirty_all()) } else { None };
        if bulk.is_some() {
            let ns = t_bulk.elapsed().as_nanos() as u64;
            self.stats.t_scan_ns += ns;
            self.stats.scan_tracked_n += 1;
            self.stats.scan_tracked_ns += ns;
        }
        for id in ids {
            let t0 = std::time::Instant::now();
            let Some(((ranges, tracked, hashed), len)) = self.mems.get_mut(&id).map(|s| (s.changed_from_bulk_or_scan(bulk.as_deref()), s.data.len())) else { continue };
            let ns = t0.elapsed().as_nanos() as u64;
            self.stats.t_scan_ns += ns;
            if tracked {
                self.stats.scan_tracked_ns += ns;
            } else {
                self.stats.scan_hashed_n += 1;
                self.stats.scan_hashed_ns += ns;
                self.stats.scan_hashed_bytes += hashed as u64;
            }
            self.stats.scan_bytes += len as u64;
            if ranges.is_empty() {
                continue;
            }
            let Some(p) = self.mems.get(&id).map(|s| s.data.as_ptr()) else { continue };
            let t1 = std::time::Instant::now();
            for (a, b) in ranges {
                // SAFETY: bufor kopii nie zmienia adresu ani rozmiaru po utworzeniu; zakres leży w jego granicach.
                let data = unsafe { std::slice::from_raw_parts(p.add(a), b - a) };
                self.send_mem(id, a as u64, data);
            }
            self.stats.t_memsend_ns += t1.elapsed().as_nanos() as u64;
        }
    }

    pub(crate) fn read_mem(&mut self, memory: u64, offset: u64, len: usize) -> Option<Vec<u8>> {
        self.flush();
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = (len - out.len()).min(MEM_CHUNK);
            self.stats.roundtrips += 1;
            self.stats.memr_bytes += n as u64;
            match self.client.call(&Request::MemRead { memory, offset: offset + out.len() as u64, len: n as u32 }) {
                Ok(Response::Data(d)) if d.len() == n => out.extend(d),
                _ => {
                    self.lose();
                    return None;
                }
            }
        }
        Some(out)
    }

    /// Czyta zakres pamięci z serwera prosto do jej lokalnej kopii (z wersjonowaniem stron po biletach odpowiedzi); `false` przy błędzie.
    pub(crate) fn read_mem_into_shadow(&mut self, memory: u64, offset: u64, len: usize) -> bool {
        // Zapisy aplikacji, jeszcze nieprzeskanowane (odroczony skan), leżą w tej samej kopii, do której zaraz wpiszemy dane z serwera: najpierw je
        // wysyłamy, inaczej pobranie okna nadpisałoby je starą zawartością (GPU czytałoby stare argumenty rysowania i zawieszało się).
        if self.mems.get(&memory).is_some_and(|s| s.lazy.is_some()) {
            self.settle_memory();
        }
        self.flush();
        let mut done = 0usize;
        while done < len {
            let n = (len - done).min(MEM_CHUNK);
            self.stats.roundtrips += 1;
            self.stats.memr_bytes += n as u64;
            let (ticket, r) = self.client.call_t(&Request::MemRead { memory, offset: offset + done as u64, len: n as u32 });
            match r {
                Ok(Response::Data(d)) if d.len() == n => {
                    if let Some(s) = self.mems.get_mut(&memory) {
                        s.apply_at(offset as usize + done, &d, ticket);
                    }
                }
                _ => {
                    self.lose();
                    return false;
                }
            }
            done += n;
        }
        true
    }

    /// Po zakończonym oczekiwaniu (fence/idle) GPU mogło zapisać odwzorowaną pamięć: dociągamy z serwera tylko zmienione strony.
    /// Bez nowego `vkQueueSubmit` od ostatniego odświeżenia nic nie mogło się zmienić, więc nie pytamy.
    fn read_mapped(&mut self) {
        let t = std::time::Instant::now();
        self.read_mapped_inner();
        self.stats.t_refresh_ns += t.elapsed().as_nanos() as u64;
    }

    /// Po zakończonym oczekiwaniu: pamięci z odczytem leniwym tylko unieważniamy (dane przyjdą przy dotknięciu przez aplikację).
    fn lazy_invalidate(&mut self) {
        let mut n = 0;
        for (id, s) in self.mems.iter() {
            let Some(r) = &s.lazy else { continue };
            if !s.mapped {
                continue;
            }
            let ranges = match self.gpuw.plan(*id) {
                crate::gpuwrite::Plan::Skip => continue,
                crate::gpuwrite::Plan::Whole => vec![(0, r.len() as u64)],
                crate::gpuwrite::Plan::Ranges(v) => v,
            };
            r.invalidate(&ranges);
            n += 1;
        }
        self.stats.lazy_invalidations += n;
    }

    /// Odwzorowane pamięci, które po oczekiwaniu trzeba odświeżyć z serwera (mapowane i dozwolone do odczytu).
    fn refresh_ids(&self) -> Vec<u64> {
        self.mems
            .iter()
            .filter(|(i, s)| s.mapped && s.lazy.is_none() && self.mem_refresh.get(i).copied().unwrap_or(true) && self.gpuw.plan(**i) != crate::gpuwrite::Plan::Skip)
            .map(|(i, _)| *i)
            .collect()
    }

    /// Wpisuje odpowiedź `MemReadDelta` do lokalnej kopii pamięci `id`; zwraca `true`, gdy serwer ma jeszcze zmiany do wysłania.
    fn apply_delta(&mut self, id: u64, d: &[u8], ticket: u64) -> bool {
        let Some((more, segs)) = gpu_proto::pagediff::decode_segments(d) else { return false };
        for (off, body) in segs {
            self.stats.memr_bytes += body.len() as u64;
            // Kopiujemy do istniejącego bufora (aplikacja trzyma wskaźnik z `vkMapMemory`), z pominięciem stron z nowszą odpowiedzią.
            if let Some(s) = self.mems.get_mut(&id) {
                s.apply_at(off as usize, body, ticket);
            }
        }
        more
    }

    /// Żądanie odświeżenia pamięci `id`: tylko zakresy, które GPU może zapisać, albo cała pamięć, gdy nie wiadomo.
    fn delta_request(&mut self, id: u64) -> Request {
        let plan = self.gpuw.plan(id);
        let size = self.mem_sizes.get(&id).copied().unwrap_or(0);
        if flags().periodic && self.plans_logged.insert(id) {
            eprintln!("[gpu-icd] powód odświeżania pamięci {id:#x}: {}", self.gpuw.describe(id));
            match &plan {
                crate::gpuwrite::Plan::Ranges(r) => eprintln!("[gpu-icd] odświeżanie pamięci {id:#x} ({} MB): {} zakresów, razem {} KB", size >> 20, r.len(), r.iter().map(|x| x.1).sum::<u64>() >> 10),
                p => eprintln!("[gpu-icd] odświeżanie pamięci {id:#x} ({} MB): {p:?}", size >> 20),
            }
        }
        match plan {
            crate::gpuwrite::Plan::Ranges(ranges) => {
                self.stats.refresh_ranged += 1;
                self.stats.refresh_ranged_bytes += ranges.iter().map(|r| r.1).sum::<u64>();
                Request::MemReadDeltaRanges { memory: id, ranges, cached: self.mem_cached.get(&id).copied().unwrap_or(false) }
            }
            _ => {
                self.stats.refresh_whole += 1;
                Request::MemReadDelta { memory: id }
            }
        }
    }

    /// Dociąga resztę zmian pamięci `id`, gdy pierwsza odpowiedź nie zmieściła się w limicie.
    fn drain_delta(&mut self, id: u64) {
        loop {
            self.flush();
            self.stats.roundtrips += 1;
            let req = self.delta_request(id);
            let (ticket, r) = self.client.call_t(&req);
            let Ok(Response::Data(d)) = r else { return };
            if !self.apply_delta(id, &d, ticket) {
                return;
            }
        }
    }

    fn read_mapped_inner(&mut self) {
        if !std::mem::take(&mut self.gpu_dirty) {
            return;
        }
        self.stats.refreshes += 1;
        for id in self.refresh_ids() {
            self.drain_delta(id);
        }
    }
}

fn state() -> Option<&'static InstanceObj> {
    // SAFETY: wskaźnik ustawia `create_instance` i zeruje `destroy_instance`; obiekt żyje między nimi.
    unsafe { CURRENT.load(std::sync::atomic::Ordering::SeqCst).as_ref() }
}

/// Punkt wejścia ICD dla polecenia o kodzie `OP` (indeks w `COMMANDS`).
///
/// # Safety
/// Wołany przez aplikację/loader z argumentami zgodnymi z sygnaturą polecenia z `vk.xml`.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn entry<const OP: u16>(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, x0: f64, x1: f64, x2: f64, x3: f64, x4: f64, x5: f64, x6: f64, x7: f64, s0: u64, s1: u64, s2: u64, s3: u64, s4: u64, s5: u64, s6: u64, s7: u64) -> u64 {
    forward(OP, [a0, a1, a2, a3, a4, a5, s0, s1, s2, s3, s4, s5, s6, s7], [x0, x1, x2, x3, x4, x5, x6, x7])
}

/// Cache wyników `vkGetDescriptorEXT` (klucz = zakodowane żądanie): jeden na proces i NIEZALEŻNY od blokady instancji, bo trafień są miliony
/// i każde brało globalną blokadę tylko po to, żeby zajrzeć do mapy. Czyszczony przy tworzeniu i niszczeniu instancji (uchwyty w kluczach
/// są numerowane od początku dla każdej instancji).
static DESC_CACHE: std::sync::RwLock<Option<std::collections::HashMap<Vec<u8>, Vec<u8>>>> = std::sync::RwLock::new(None);
static DESC_LOCKFREE_SYNTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DESC_LOCKFREE_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn desc_cache_clear() {
    *DESC_CACHE.write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Kopiuje zapamiętany deskryptor pod `out`, jeśli jest w cache i ma dokładnie `size` bajtów.
///
/// # Safety
/// `out` musi być zapisywalne na `size` bajtów.
unsafe fn desc_cache_hit(key: &[u8], out: u64, size: u64) -> bool {
    let g = DESC_CACHE.read().unwrap_or_else(|e| e.into_inner());
    match g.as_ref().and_then(|m| m.get(key)) {
        Some(v) if v.len() as u64 == size => {
            std::ptr::copy_nonoverlapping(v.as_ptr(), out as *mut u8, v.len());
            true
        }
        _ => false,
    }
}

fn desc_cache_store(key: Vec<u8>, value: Vec<u8>) {
    let mut g = DESC_CACHE.write().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(Default::default);
    if m.len() >= DESC_CACHE_MAX_ENTRIES {
        m.clear();
    }
    m.insert(key, value);
}

/// Odpowiedź na `vkGetDescriptorEXT` BEZ blokady instancji: wzór bufora (sprawdzony, `descsynth`) albo trafienie w cache. `None` = zwykła ścieżka.
///
/// # Safety
/// Argumenty muszą pochodzić z wywołania aplikacji (device, pDescriptorInfo, dataSize, pDescriptor).
unsafe fn descriptor_lockfree(op: u16, cmd: &gpu_proto::meta::CommandMeta, ints: &[u64; 14], floats: &[f64; 8]) -> bool {
    let (info, size, out) = (ints[1], ints[2], ints[3]);
    if out == 0 || size > DESC_CACHE_MAX_BYTES || state().is_none() {
        return false;
    }
    if let Some((ty, address, range)) = crate::descsynth::input_of(info as *const u8) {
        if let Some(d) = crate::descsynth::Synth::try_lockfree(ty, size as usize, address, range) {
            std::ptr::copy_nonoverlapping(d.as_ptr(), out as *mut u8, d.len());
            DESC_LOCKFREE_SYNTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return true;
        }
    }
    let params = cmd.params();
    let mut slots = [0u64; 8];
    if params.len() > slots.len() {
        return false;
    }
    fill_slots(params, ints, floats, &mut slots);
    let mut e = Encoder::new();
    if encode_command(op, slots.as_ptr() as *const u8, &mut e, &IcdCtx).is_err() {
        return false;
    }
    if desc_cache_hit(&e.finish(), out, size) {
        DESC_LOCKFREE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return true;
    }
    false
}

/// Numer polecenia `vkGetDescriptorEXT` (ustalany raz).
fn descriptor_op() -> u16 {
    static OP: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    *OP.get_or_init(|| gpu_proto::meta::find_command("vkGetDescriptorEXT").map_or(u16::MAX, |(op, _)| op))
}

/// Pomiar blokady instancji: łączny czas oczekiwania na nią, czas trzymania i liczba wejść (widoczne w statystykach).
/// Pokazuje, czy wątki gry czekają jeden na drugiego (np. gdy `vkQueueSubmit` skanuje i wysyła pamięć pod blokadą).
static LOCK_WAIT_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOCK_HOLD_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOCK_ENTRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Najdłuższe pojedyncze trzymanie blokady w ns (ogon opóźnień).
static LOCK_HOLD_MAX_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Liczba poleceń obsłużonych szybką ścieżką (bez blokady instancji).
static FAST_COMMANDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct HoldTimer(std::time::Instant);

impl Drop for HoldTimer {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let ns = self.0.elapsed().as_nanos() as u64;
        LOCK_HOLD_NS.fetch_add(ns, Relaxed);
        LOCK_HOLD_MAX_NS.fetch_max(ns, Relaxed);
    }
}

/// Opis pomiaru blokady do statystyk.
pub fn lock_report() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    format!(
        "blokada instancji: oczekiwanie {:.0} ms, trzymanie {:.0} ms w {} wejściach, najdłuższe trzymanie {:.1} ms, szybka ścieżka {} poleceń, deskryptory bez blokady: lokalnie {}, z cache {}",
        LOCK_WAIT_NS.load(Relaxed) as f64 / 1e6,
        LOCK_HOLD_NS.load(Relaxed) as f64 / 1e6,
        LOCK_ENTRIES.load(Relaxed),
        LOCK_HOLD_MAX_NS.load(Relaxed) as f64 / 1e6,
        FAST_COMMANDS.load(Relaxed),
        DESC_LOCKFREE_SYNTH.load(Relaxed),
        DESC_LOCKFREE_HITS.load(Relaxed)
    )
}

/// Szybka ścieżka poleceń zapisu do bufora poleceń (`vkCmd*`): ponad 99% wejść do sterownika, a globalna blokada instancji była zajęta w ok. 74%
/// czasu (skan pamięci, wysyłka, czekanie na serwer), więc wątek gry stał w kolejce. Polecenia są kodowane do bufora **danego bufora poleceń**
/// (`DispObj::pending`) pod jego własnym mutexem, a do wspólnej partii trafiają przy `vkEndCommandBuffer` (i przy progu rozmiaru).
/// Po tylu bajtach bufor poleceń jest przenoszony do wspólnej partii (żeby nie rósł bez końca między `vkEndCommandBuffer`).
const LOCAL_FLUSH_BYTES: usize = 256 * 1024;

/// Które polecenia mogą iść szybką ścieżką: `vkCmd*` bez wyniku i bez parametrów wyjściowych, klasy FireAndForget. Żadne z nich nie ma
/// obsługi po nazwie w `exec` (wszystkie wyjątki to `vkCreate*`, `vkAllocate*`, `vkBind*`, `vkQueueSubmit*` itd.).
fn fast_table() -> &'static [bool] {
    static T: std::sync::OnceLock<Vec<bool>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        COMMANDS
            .iter()
            .map(|c| {
                c.class == CmdClass::FireAndForget
                    && c.name.starts_with("vkCmd")
                    // Wykonanie buforów wtórnych zależy od ich zawartości na serwerze: zawsze zwykłą ścieżką (przegląd agy).
                    && c.name != "vkCmdExecuteCommands"
                    && c.ret == Ret::Void
                    && c.params().first().is_some_and(|p| p.type_name == "VkCommandBuffer" && p.elem == Elem::Handle)
                    && !c.params().iter().any(|p| p.shape == Shape::Pointer && !p.is_const && p.elem != Elem::Handle && p.ptr_depth == 1)
            })
            .collect()
    })
}

/// Zamienia argumenty wejścia ICD na sloty (odwrotność thunka serwera): liczby zmiennoprzecinkowe z rejestrów xmm, reszta z rejestrów całkowitych i stosu.
fn fill_slots(params: &[gpu_proto::meta::MemberMeta], ints: &[u64; 14], floats: &[f64; 8], slots: &mut [u64]) {
    let (mut ii, mut fi) = (0, 0);
    for (k, p) in params.iter().enumerate() {
        slots[k] = match (p.shape, p.elem, p.type_name) {
            (Shape::Value, Elem::Scalar, "float") => {
                fi += 1;
                floats.get(fi - 1).map_or(0, |f| f.to_bits() & 0xFFFF_FFFF)
            }
            (Shape::Value, Elem::Scalar, "double") => {
                fi += 1;
                floats.get(fi - 1).map_or(0, |f| f.to_bits())
            }
            _ => {
                ii += 1;
                ints.get(ii - 1).copied().unwrap_or(0)
            }
        };
    }
}

/// Koduje polecenie do bufora tego wątku bez blokady. `false` = nie obsłużono (idzie zwykłą ścieżką).
///
/// # Safety
/// Argumenty muszą pochodzić z wywołania aplikacji zgodnego z sygnaturą polecenia.
unsafe fn fast_record(op: u16, cmd: &gpu_proto::meta::CommandMeta, ints: &[u64; 14], floats: &[f64; 8]) -> bool {
    let params = cmd.params();
    let mut slots = [0u64; 32];
    if params.len() > slots.len() || state().is_none() {
        return false;
    }
    fill_slots(params, ints, floats, &mut slots);
    let mut e = Encoder::new();
    if encode_command(op, slots.as_ptr() as *const u8, &mut e, &IcdCtx).is_err() {
        return false; // błąd zgłosi zwykła ścieżka (z komunikatem w logu)
    }
    let bytes = e.finish();
    // Pierwszy parametr każdego `vkCmd*` to bufor poleceń: wskaźnik na nasz `DispObj`.
    let cb = slots[0] as *const DispObj;
    if cb.is_null() {
        return false;
    }
    // SAFETY: uchwyt bufora poleceń to wskaźnik na obiekt ICD żyjący do `vkFreeCommandBuffers`/zniszczenia puli.
    if (*cb).push(&bytes) {
        let data = (*cb).take();
        if let Some(inst) = state() {
            let mut st = lock_state(inst);
            st.batch.extend_from_slice(&data);
            st.stats.commands += 1;
            if st.batch.len() > BATCH_FLUSH {
                st.flush();
            }
        }
    }
    FAST_COMMANDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    true
}

/// Przenosi zebrane polecenia bufora poleceń `cb` do wspólnej partii (pod blokadą stanu), przed `vkEndCommandBuffer` i podobnymi.
///
/// # Safety
/// `cb` musi być zerem albo wskaźnikiem na żywy `DispObj` bufora poleceń.
unsafe fn drain_cb(st: &mut InstState, cb: u64) {
    if cb == 0 {
        return;
    }
    let data = (*(cb as *const DispObj)).take();
    if !data.is_empty() {
        st.batch.extend_from_slice(&data);
    }
}

/// Blokada stanu instancji.
pub(crate) fn lock_state(inst: &InstanceObj) -> std::sync::MutexGuard<'_, InstState> {
    lock(&inst.state)
}

unsafe fn forward(op: u16, ints: [u64; 14], floats: [f64; 8]) -> u64 {
    let Some(cmd) = COMMANDS.get(op as usize) else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    if op == descriptor_op() && descriptor_lockfree(op, cmd, &ints, &floats) {
        return 0;
    }
    if flags().fastpath && fast_table().get(op as usize).copied().unwrap_or(false) && !flags().trace && fast_record(op, cmd, &ints, &floats) {
        return 0;
    }
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    crate::watchdog::set(op, crate::watchdog::WAIT_LOCK, 0);
    let _wd = WdGuard;
    let t_lock = std::time::Instant::now();
    let mut st = lock_state(inst);
    crate::watchdog::phase(crate::watchdog::HOLD_LOCK, 0);
    LOCK_WAIT_NS.fetch_add(t_lock.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    LOCK_ENTRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut hold = HoldTimer(std::time::Instant::now());
    let fail = |e: i32| -> u64 { e as u32 as u64 };
    if st.device_lost && cmd.ret == Ret::VkResult {
        return fail(VK_ERROR_DEVICE_LOST);
    }
    // Argumenty -> sloty (odwrotność thunka serwera).
    let params = cmd.params();
    let mut slots = vec![0u64; params.len()];
    fill_slots(params, &ints, &floats, &mut slots);
    // `vkUpdateDescriptorSets`: spec każe czytać tylko ten ze wskaźników pImageInfo/pBufferInfo/pTexelBufferView, który odpowiada typowi
    // deskryptora; pozostałe mogą zawierać śmieci (Tellusim/GravityMark ich nie zeruje), a kodek dereferencjonuje każdy niezerowy wskaźnik
    // i wywraca grę (SIGSEGV w `enc_struct`). Kodujemy kopię zapisów z wyzerowanymi nieużywanymi wskaźnikami (pamięć aplikacji zostaje nietknięta).
    let mut _write_scratch: Vec<u8> = Vec::new();
    if cmd.name == "vkUpdateDescriptorSets" && slots.len() >= 3 && slots[1] != 0 && slots[1] <= 65536 && slots[2] != 0 {
        let n = slots[1] as usize;
        _write_scratch = sanitize_descriptor_writes(std::slice::from_raw_parts(slots[2] as *const u8, n * 64));
        slots[2] = _write_scratch.as_ptr() as u64;
    }
    match cmd.name {
        // Zamknięcie nagrywania: wszystko, co ten bufor zebrał, idzie do wspólnej partii PRZED `vkEndCommandBuffer`.
        "vkEndCommandBuffer" => drain_cb(&mut st, slots[0]),
        // Początek nowego nagrywania albo reset: zaległości z przerwanego nagrywania (np. po `vkResetCommandPool`) są śmieciami i
        // nie mogą trafić do strumienia przed nowym `Begin` (serwer dostałby `vkCmd*` dla bufora, który nie nagrywa).
        "vkBeginCommandBuffer" | "vkResetCommandBuffer" if slots[0] != 0 => drop((*(slots[0] as *const DispObj)).take()),
        // Każde inne `vkCmd*`, które nie poszło szybką ścieżką, wchodzi do wspólnej partii natychmiast, więc zaległe polecenia TEGO bufora
        // muszą je poprzedzić (kolejność nagrywania w obrębie bufora).
        n if n.starts_with("vkCmd") && slots[0] != 0 && params.first().is_some_and(|p| p.type_name == "VkCommandBuffer") => drain_cb(&mut st, slots[0]),
        _ => {}
    }
    if cmd.name == "vkCreateGraphicsPipelines" && flags().trace {
        for i in 0..slots.get(2).copied().unwrap_or(0).min(8) as usize {
            let info = (slots[3] as *const u8).add(i * 144);
            trace_text(&format!("  pipeline[{i}] app: {}", gpu_proto::debug::describe_graphics_pipeline_info(info)));
        }
    }
    let t0 = std::time::Instant::now();
    // Blokujące oczekiwania (`vkWaitSemaphores`, `vkWaitForFences`) wysyłamy do serwera w krótkich odcinkach (1 -> 4 ms) i powtarzamy do upływu
    // limitu aplikacji. Serwer wykonuje żądania po kolei, więc długie oczekiwanie blokowałoby przetwarzanie kolejnych żądań, w tym zgłoszenia pracy,
    // na którą to oczekiwanie czeka (vkd3d czeka na wartość semafora, którą inny wątek zgłosi dopiero potem): zablokowany strumień, gra stoi do
    // upływu limitu (do 16 s z blokadą w ręku), a silnik gry kończy się własnym watchdogiem po 120 s.
    let wait_idx = if flags().slice_wait && matches!(cmd.name, "vkWaitSemaphores" | "vkWaitSemaphoresKHR" | "vkWaitForFences") { params.iter().position(|p| p.name == "timeout") } else { None };
    let wait_total = wait_idx.map(|i| slots[i]);
    let wait_start = std::time::Instant::now();
    let mut slice_ns: u64 = 1_000_000;
    let r = loop {
        if let (Some(i), Some(total)) = (wait_idx, wait_total) {
            let elapsed = wait_start.elapsed().as_nanos() as u64;
            let remaining = if total == u64::MAX { u64::MAX } else { total.saturating_sub(elapsed) };
            slots[i] = if total == 0 { 0 } else { remaining.min(slice_ns) };
        }
        let r = match exec_begin(&mut st, op, &mut slots, false) {
            Step::Done(v) => v,
            Step::Pending(p) if flags().locked_wait => {
                let resp = p.receive();
                exec_finish(&mut st, p, resp)
            }
            Step::Pending(p) => {
                // Czekamy na odpowiedź serwera BEZ blokady instancji: pozostałe wątki gry nagrywają, zgłaszają i pytają w tym czasie.
                // Wykonanie u serwera i tak jest sekwencyjne, a odpowiedzi odbieramy w kolejności biletów (patrz `link`).
                drop(hold);
                drop(st);
                crate::watchdog::phase(crate::watchdog::WAIT_REPLY, p.base);
                let resp = p.receive();
                crate::watchdog::phase(crate::watchdog::WAIT_LOCK_FINISH, p.base);
                let t_lock = std::time::Instant::now();
                st = lock_state(inst);
                crate::watchdog::phase(crate::watchdog::HOLD_LOCK, p.base);
                LOCK_WAIT_NS.fetch_add(t_lock.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
                LOCK_ENTRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                hold = HoldTimer(std::time::Instant::now());
                exec_finish(&mut st, p, resp)
            }
        };
        // Odcinek upłynął bez spełnienia warunku (VK_TIMEOUT = 2), a limit aplikacji jeszcze nie: odpuszczamy blokadę na chwilę i pytamy ponownie.
        let timed_out = r as u32 as i32 == 2;
        let more = match (wait_idx, wait_total) {
            (Some(_), Some(total)) => total != 0 && (total == u64::MAX || (wait_start.elapsed().as_nanos() as u64) < total),
            _ => false,
        };
        if !(timed_out && more) {
            break r;
        }
        drop(hold);
        drop(st);
        std::thread::sleep(std::time::Duration::from_micros(50));
        st = lock_state(inst);
        hold = HoldTimer(std::time::Instant::now());
        slice_ns = (slice_ns * 2).min(4_000_000);
    };
    let _ = &hold;
    trace(cmd.name, r, t0.elapsed().as_micros());
    periodic_stats(&st.stats);
    r
}

/// Kopia tablicy `VkWriteDescriptorSet` (64 B każdy) z wyzerowanymi wskaźnikami, których typ deskryptora nie używa (patrz `forward`).
fn sanitize_descriptor_writes(src: &[u8]) -> Vec<u8> {
    let mut out = src.to_vec();
    for w in out.as_chunks_mut::<64>().0 {
        let ty = u32::from_le_bytes([w[36], w[37], w[38], w[39]]);
        // VkWriteDescriptorSet: pImageInfo @40, pBufferInfo @48, pTexelBufferView @56.
        let (keep_image, keep_buffer, keep_texel) = match ty {
            0..=3 | 10 => (true, false, false),
            4 | 5 => (false, false, true),
            6..=9 => (false, true, false),
            _ => (true, true, true), // typy z rozszerzeń (inline uniform block, struktury przyspieszenia): wskaźniki bez zmian
        };
        for (keep, at) in [(keep_image, 40usize), (keep_buffer, 48), (keep_texel, 56)] {
            if !keep {
                w[at..at + 8].fill(0);
            }
        }
    }
    out
}

/// Zapamiętuje, co przypisano do pamięci (bufory: zakres i użycie; obrazy: cała pamięć), żeby po oczekiwaniu odświeżać tylko to,
/// co GPU może zapisać (patrz `gpuwrite`).
/// SAFETY: `slots` pochodzi z wywołania aplikacji: wskaźniki `pBindInfos` muszą być ważne dla `bindInfoCount` elementów.
unsafe fn note_memory_binding(st: &mut InstState, name: &str, slots: &[u64]) {
    match name {
        // vkBindBufferMemory(device, buffer, memory, memoryOffset)
        "vkBindBufferMemory" if slots.len() >= 4 => st.gpuw.bind_buffer(slots[2], slots[1], slots[3]),
        "vkBindImageMemory" if slots.len() >= 3 => st.gpuw.bind_image(slots[2]),
        // VkBindBufferMemoryInfo / VkBindImageMemoryInfo: sType, pNext, zasób@16, pamięć@24, offset@32 (40 B).
        "vkBindBufferMemory2" | "vkBindBufferMemory2KHR" | "vkBindImageMemory2" | "vkBindImageMemory2KHR" if slots.len() >= 3 && slots[2] != 0 => {
            let buffers = name.starts_with("vkBindBuffer");
            for i in 0..slots[1] as usize {
                let base = (slots[2] as *const u8).add(i * 40);
                let resource = std::ptr::read_unaligned(base.add(16) as *const u64);
                let memory = std::ptr::read_unaligned(base.add(24) as *const u64);
                let offset = std::ptr::read_unaligned(base.add(32) as *const u64);
                if buffers {
                    st.gpuw.bind_buffer(memory, resource, offset);
                } else {
                    st.gpuw.bind_image(memory);
                }
            }
        }
        _ => {}
    }
}

/// Co 5 s (przy `GPU_ICD_DEBUG`) wypisuje liczniki sieci do logu: pokazuje, czy ładowanie ogranicza przepustowość pamięci.
fn periodic_stats(stats: &Stats) {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if !flags().periodic {
        return;
    }
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if last.is_none_or(|t| now.duration_since(t).as_secs() >= 5) {
        *last = Some(now);
        eprintln!("[gpu-icd] {}", stats.report());
        eprintln!("[gpu-icd] obiegi wg poleceń: {}", stats.top_roundtrips());
        eprintln!("[gpu-icd] wysyłki pamięci wg obiektów: {}", stats.top_uploads());
        if stats.lazy_memories > 0 {
            use std::sync::atomic::Ordering::Relaxed;
            eprintln!("[gpu-icd] odczyt leniwy: pamięci {}, unieważnienia {}, błędy stron {} (z pobraniem {}, {} KB), przekroczenia czasu {}", stats.lazy_memories, stats.lazy_invalidations, crate::lazymap::FAULTS.load(Relaxed), crate::lazymap::FAULTS_FETCHED.load(Relaxed), crate::lazymap::FETCHED_BYTES.load(Relaxed) / 1024, crate::lazymap::FETCH_TIMEOUTS.load(Relaxed));
        }
        eprintln!("[gpu-icd] {}", lock_report());
        if let Some(l) = crate::latency::report_and_reset() {
            eprintln!("[gpu-icd] {l}");
        }
        trace_flush();
    }
}

/// Plik śladu otwarty raz i buforowany (poprzednio: otwarcie, zapis i zamknięcie przy KAŻDYM poleceniu).
fn trace_sink() -> Option<&'static std::sync::Mutex<std::io::BufWriter<std::fs::File>>> {
    static SINK: std::sync::OnceLock<Option<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>> = std::sync::OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var("GPU_ICD_TRACE").ok()?;
        let f = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(std::sync::Mutex::new(std::io::BufWriter::with_capacity(1 << 20, f)))
    })
    .as_ref()
}

fn trace_text(line: &str) {
    if !flags().trace {
        return;
    }
    if let Some(sink) = trace_sink() {
        use std::io::Write;
        let _ = writeln!(sink.lock().unwrap_or_else(|e| e.into_inner()), "{line}");
    }
}

/// Zapisuje bufor śladu na dysk (wołane z cyklicznych liczników i przy zniszczeniu instancji).
pub fn trace_flush() {
    if let Some(sink) = trace_sink() {
        use std::io::Write;
        let _ = sink.lock().unwrap_or_else(|e| e.into_inner()).flush();
    }
}

/// `GPU_ICD_TRACE=<plik>`: dopisuje nazwę każdego polecenia aplikacji i zwróconą wartość (diagnostyka awarii gier).
fn trace(name: &str, ret: u64, micros: u128) {
    if !flags().trace {
        return;
    }
    if let Some(sink) = trace_sink() {
        use std::io::Write;
        let _ = writeln!(sink.lock().unwrap_or_else(|e| e.into_inner()), "{name} -> {:#x} [{micros}us]", ret as u32 as i32);
    }
}

/// Alokacje, które w normalnym działaniu mogą się nie powieść i których aplikacja oczekuje wyniku (pełna pula deskryptorów =>
/// nowa pula i ponowna próba; brak pamięci => inny typ/heap), więc nie mogą być optymistyczne.
fn may_fail_legitimately(name: &str) -> bool {
    matches!(name, "vkAllocateDescriptorSets" | "vkAllocateMemory")
}

/// `VK_PIPELINE_CREATE_FAIL_ON_PIPELINE_COMPILE_REQUIRED_BIT`: aplikacja pyta, czy potok jest w pamięci podręcznej, i oczekuje
/// prawdziwego wyniku (`VK_PIPELINE_COMPILE_REQUIRED`), więc takie tworzenie nie może być optymistyczne.
const PIPELINE_FAIL_ON_COMPILE_REQUIRED: u32 = 0x100;

/// Czy `vkCreate*Pipelines` zawiera choć jedną strukturę z flagą `FAIL_ON_PIPELINE_COMPILE_REQUIRED` (wymaga synchronicznej odpowiedzi).
///
/// # Safety
/// `slots[2]` i `slots[3]` muszą opisywać `createInfoCount` i `pCreateInfos` zgodnie z deklaracją polecenia.
unsafe fn is_pipeline_probe(name: &str, slots: &[u64]) -> bool {
    let info = match name {
        "vkCreateGraphicsPipelines" => "VkGraphicsPipelineCreateInfo",
        "vkCreateComputePipelines" => "VkComputePipelineCreateInfo",
        "vkCreateRayTracingPipelinesKHR" => return pipeline_infos_have_flag(slots, 3, 4, "VkRayTracingPipelineCreateInfoKHR"),
        _ => return false,
    };
    pipeline_infos_have_flag(slots, 2, 3, info)
}

unsafe fn pipeline_infos_have_flag(slots: &[u64], count_slot: usize, ptr_slot: usize, struct_name: &str) -> bool {
    let (Some(&count), Some(&ptr)) = (slots.get(count_slot), slots.get(ptr_slot)) else { return false };
    let Some(size) = gpu_proto::meta::find_struct(struct_name).map(|m| m.size as usize) else { return false };
    if ptr == 0 || count > 4096 {
        return false;
    }
    (0..count as usize).any(|i| std::ptr::read_unaligned((ptr as *const u8).add(i * size + 16) as *const u32) & PIPELINE_FAIL_ON_COMPILE_REQUIRED != 0)
}

/// Pamięci do tego rozmiaru zawsze odświeżamy po oczekiwaniu (tanie, a aplikacja może czytać wynik z dowolnego typu).
const REFRESH_ALWAYS_BELOW: u64 = 16 * 1024 * 1024;

/// Czy typ pamięci ma HOST_CACHED (0x8): z takiej pamięci aplikacje czytają wyniki GPU i ona jest szybka do odczytu.
/// Duże pamięci bez tej flagi to sterty wysyłkowe (write-combined), z których CPU praktycznie nie czyta (natywnie też jest to wolne).
/// Czy typ pamięci przydzielanej przez `vkAllocateMemory` ma HOST_CACHED; `None`, gdy typu nie da się ustalić.
unsafe fn memory_is_cached(st: &mut InstState, slots: &[u64], cmd: &gpu_proto::meta::CommandMeta, args: *const u8) -> Option<bool> {
    let idx = gpu_vkcodec::codec::arg_uint(cmd, args, "pAllocateInfo->memoryTypeIndex")?;
    let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
    let pdev = st.device_pdev.get(&dev).copied()?;
    let props = st.query_props(pdev)?;
    let at = 20 + idx as usize * 8;
    props.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).map(|f| {
        LAST_MEMORY_FLAGS.store(f, std::sync::atomic::Ordering::Relaxed);
        f & 0x8 != 0
    })
}

/// Flagi właściwości typu pamięci ostatnio alokowanej (do księgowania w `mem_flags`; zapis i odczyt pod blokadą instancji).
static LAST_MEMORY_FLAGS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

const STYPE_MEMORY_DEDICATED_REQUIREMENTS: u32 = 1_000_127_000;

/// Rozpoznaje zapytania o wymagania pamięci: (obiekt, wskaźnik na `VkMemoryRequirements`, wskaźnik na ewentualne
/// `VkMemoryDedicatedRequirements` w odpowiedzi). `None`, gdy zapytanie ma inne rozszerzenia (nie cache'ujemy).
unsafe fn memreq_target(name: &str, slots: &[u64]) -> Option<(u64, *mut u8, Option<*mut u8>)> {
    let two = name.ends_with('2');
    match name {
        "vkGetImageMemoryRequirements" | "vkGetImageMemoryRequirements2" | "vkGetBufferMemoryRequirements" | "vkGetBufferMemoryRequirements2" => {}
        _ => return None,
    }
    if slots.len() < 3 || slots[2] == 0 {
        return None;
    }
    if !two {
        return Some((slots[1], slots[2] as *mut u8, None));
    }
    // pInfo: sType @0, pNext @8 (musi być puste), obiekt @16; wynik: VkMemoryRequirements2 (dane @16, pNext @8).
    let (info, out) = (slots[1] as *const u8, slots[2] as *mut u8);
    if info.is_null() || std::ptr::read_unaligned(info.add(8) as *const u64) != 0 {
        return None;
    }
    let next = std::ptr::read_unaligned(out.add(8) as *const u64) as *mut u8;
    let dedicated = if next.is_null() {
        None
    } else {
        // Jedyny dozwolony element łańcucha odpowiedzi: VkMemoryDedicatedRequirements bez dalszego pNext.
        if std::ptr::read_unaligned(next as *const u32) != STYPE_MEMORY_DEDICATED_REQUIREMENTS || std::ptr::read_unaligned(next.add(8) as *const u64) != 0 {
            return None;
        }
        Some(next)
    };
    Some((std::ptr::read_unaligned(info.add(16) as *const u64), out.add(16), dedicated))
}

/// Odpowiedź z cache; `Some(0)` oznacza, że wynik został wpisany i polecenie jest załatwione.
unsafe fn memreq_cached(st: &mut InstState, name: &str, slots: &[u64]) -> Option<u64> {
    let (obj, out, dedicated) = memreq_target(name, slots)?;
    let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
    let (size, align, bits, ded) = st.memreq.lookup(dev, obj, dedicated.is_some())?;
    std::ptr::write_unaligned(out as *mut u64, size);
    std::ptr::write_unaligned(out.add(8) as *mut u64, align);
    std::ptr::write_unaligned(out.add(16) as *mut u32, bits);
    if let (Some(node), Some((prefers, requires))) = (dedicated, ded) {
        std::ptr::write_unaligned(node.add(16) as *mut u32, prefers);
        std::ptr::write_unaligned(node.add(20) as *mut u32, requires);
    }
    Some(0)
}

/// `vkGetDeviceImageMemoryRequirements[KHR](device, pInfo, pMemoryRequirements)`: (klucz parametrów, wynik `VkMemoryRequirements`, opcjonalne
/// `VkMemoryDedicatedRequirements`). `None`, gdy zapytanie nie jest tym poleceniem albo ma nieznane rozszerzenia.
unsafe fn devreq_target(name: &str, slots: &[u64]) -> Option<(Vec<u8>, *mut u8, Option<*mut u8>)> {
    if !matches!(name, "vkGetDeviceImageMemoryRequirements" | "vkGetDeviceImageMemoryRequirementsKHR") || slots.len() < 3 || slots[1] == 0 || slots[2] == 0 {
        return None;
    }
    let key = crate::memreq::MemReqCache::device_image_key(slots[1] as *const u8)?;
    let out = slots[2] as *mut u8;
    let next = std::ptr::read_unaligned(out.add(8) as *const u64) as *mut u8;
    let dedicated = if next.is_null() {
        None
    } else {
        if std::ptr::read_unaligned(next as *const u32) != STYPE_MEMORY_DEDICATED_REQUIREMENTS || std::ptr::read_unaligned(next.add(8) as *const u64) != 0 {
            return None;
        }
        Some(next)
    };
    Some((key, out.add(16), dedicated))
}

unsafe fn devreq_cached(st: &mut InstState, name: &str, slots: &[u64]) -> Option<u64> {
    let (key, out, dedicated) = devreq_target(name, slots)?;
    let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
    let (size, align, bits, ded) = st.memreq.lookup_key(dev, &key, dedicated.is_some())?;
    std::ptr::write_unaligned(out as *mut u64, size);
    std::ptr::write_unaligned(out.add(8) as *mut u64, align);
    std::ptr::write_unaligned(out.add(16) as *mut u32, bits);
    if let (Some(node), Some((prefers, requires))) = (dedicated, ded) {
        std::ptr::write_unaligned(node.add(16) as *mut u32, prefers);
        std::ptr::write_unaligned(node.add(20) as *mut u32, requires);
    }
    Some(0)
}

unsafe fn devreq_store(st: &mut InstState, name: &str, slots: &[u64]) {
    let Some((key, out, dedicated)) = devreq_target(name, slots) else { return };
    let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
    let size = std::ptr::read_unaligned(out as *const u64);
    let align = std::ptr::read_unaligned(out.add(8) as *const u64);
    let bits = std::ptr::read_unaligned(out.add(16) as *const u32);
    let ded = dedicated.map(|n| (std::ptr::read_unaligned(n.add(16) as *const u32), std::ptr::read_unaligned(n.add(20) as *const u32)));
    st.memreq.store_key(dev, key, (size, align, bits, ded));
}

/// Po prawdziwej odpowiedzi serwera zapamiętuje wynik dla klucza tego obiektu.
unsafe fn memreq_store(st: &mut InstState, name: &str, slots: &[u64]) {
    let Some((obj, out, dedicated)) = memreq_target(name, slots) else { return };
    let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
    let size = std::ptr::read_unaligned(out as *const u64);
    let align = std::ptr::read_unaligned(out.add(8) as *const u64);
    let bits = std::ptr::read_unaligned(out.add(16) as *const u32);
    let ded = dedicated.map(|n| (std::ptr::read_unaligned(n.add(16) as *const u32), std::ptr::read_unaligned(n.add(20) as *const u32)));
    st.memreq.store(dev, obj, (size, align, bits, ded));
}

/// Rozwija `vkUpdateDescriptorSetWithTemplate(device, set, template, pData)` do `vkUpdateDescriptorSets` z jawnymi zapisami.
unsafe fn update_with_template(st: &mut InstState, slots: &[u64]) -> u64 {
    let (device, set, template, data) = (slots[0], slots[1], slots[2], slots[3] as *const u8);
    let Some(entries) = st.templates.get(&template).cloned() else { return 0 };
    let Some(w) = crate::templates::build_writes(&entries, data, set) else {
        eprintln!("gpu-icd: szablon deskryptorów z nieobsługiwanym typem (inline uniform block?)");
        return 0;
    };
    let Some((op, _)) = gpu_proto::meta::find_command("vkUpdateDescriptorSets") else { return 0 };
    if w.count == 0 {
        return 0;
    }
    let mut args = [device, w.count as u64, w.bytes.as_ptr() as u64, 0, 0];
    exec(st, op, &mut args)
}

/// Wykonuje polecenie o kodzie `op` z gotowymi slotami parametrów (tak samo dla aplikacji i dla ICD, które wydaje własne polecenia).
///
/// # Safety
/// Sloty muszą zawierać poprawne wartości parametrów polecenia (wskaźniki do żywej pamięci).
pub(crate) unsafe fn exec(st: &mut InstState, op: u16, slots: &mut [u64]) -> u64 {
    exec_with(st, op, slots, false)
}

/// Jak `exec`; `force_async` wymusza drogę partii (bez odpowiedzi) także dla poleceń z oczekiwaniem (np. `vkWaitForFences`),
/// gdy wynik nie jest potrzebny, a serwer ma je tylko wykonać w kolejności.
///
/// # Safety
/// Jak `exec`.
pub(crate) unsafe fn exec_begin(st: &mut InstState, op: u16, slots: &mut [u64], force_async: bool) -> Step {
    let Some(cmd) = COMMANDS.get(op as usize) else { return Step::Done(VK_ERROR_INITIALIZATION_FAILED as u32 as u64) };
    let params = cmd.params();
    let fail = |e: i32| -> u64 { e as u32 as u64 };
    let args = slots.as_ptr() as *const u8;

    // Księgowość deskryptorów: układy i pule zapamiętujemy, a alokację zestawów odpowiadamy od razu tylko wtedy, gdy pula na pewno starczy.
    let mut speculated = false;
    let mut audited = false;
    let mut alloc_ctx: Option<(u64, Vec<u64>)> = None;
    match cmd.name {
        "vkResetDescriptorPool" => st.desc.pool_reset(slots[1]),
        "vkDestroyDescriptorPool" => st.desc.pool_destroyed(slots[1]),
        "vkDestroyDescriptorSetLayout" => st.desc.layout_destroyed(slots[1]),
        "vkAllocateDescriptorSets" if slots[1] != 0 => {
            let info = slots[1] as *const u8;
            let pnext = std::ptr::read_unaligned(info.add(8) as *const u64) != 0;
            let pool = std::ptr::read_unaligned(info.add(16) as *const u64);
            let n = std::ptr::read_unaligned(info.add(24) as *const u32).min(4096) as usize;
            let lp = std::ptr::read_unaligned(info.add(32) as *const u64) as *const u64;
            let layouts: Vec<u64> = if lp.is_null() { Vec::new() } else { (0..n).map(|i| std::ptr::read_unaligned(lp.add(i))).collect() };
            let predicted = flags().desc_spec && !DESC_SPEC_BROKEN.load(std::sync::atomic::Ordering::Relaxed) && st.desc.would_fit(pool, &layouts, pnext);
            audited = predicted && {
                let n = DESC_SPEC_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                n.is_multiple_of(DESC_SPEC_AUDIT_EVERY) || (n < 256 && n.is_multiple_of(8))
            };
            speculated = predicted && !audited && st.desc.try_allocate(pool, &layouts, pnext);
            alloc_ctx = Some((pool, layouts));
        }
        _ => {}
    }
    // Wymagania pamięci: identyczne obrazy i bufory dostają odpowiedź z cache, bez obiegu po sieci.
    if let Some(done) = memreq_cached(st, cmd.name, slots) {
        return Step::Done(done);
    }
    if let Some(done) = devreq_cached(st, cmd.name, slots) {
        return Step::Done(done);
    }
    // Deskryptory buforów (UNIFORM/STORAGE) liczone lokalnie, gdy wzór został sprawdzony na odpowiedziach serwera (patrz `descsynth`).
    if cmd.name == "vkGetDescriptorEXT" && slots.len() >= 4 && slots[3] != 0 {
        if let Some((ty, address, range)) = crate::descsynth::input_of(slots[1] as *const u8) {
            if let Some(d) = st.synth.try_local(ty, slots[2] as usize, address, range) {
                std::ptr::copy_nonoverlapping(d.as_ptr(), slots[3] as *mut u8, d.len());
                st.stats.desc_synth += 1;
                return Step::Done(0);
            }
        }
    }
    // Aktualizacja deskryptorów szablonem: dane zawierają uchwyty, więc rozwijamy ją do zwykłego vkUpdateDescriptorSets.
    if cmd.name == "vkUpdateDescriptorSetWithTemplate" {
        return Step::Done(update_with_template(st, slots));
    }
    // Kolejka wydana już wcześniej (to samo urządzenie, rodzina, indeks) wraca bez wysyłania.
    if cmd.name == "vkGetDeviceQueue" {
        let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
        if let Some(q) = st.queues.get(&(dev, slots[1] as u32, slots[2] as u32)) {
            std::ptr::write_unaligned(slots[3] as *mut usize, *q);
            return Step::Done(0);
        }
    }
    // Uchwyty wyjściowe nadaje klient: wpisujemy identyfikatory w pamięć aplikacji przed zakodowaniem.
    let mut disp_outs: Vec<(*mut u64, u64)> = Vec::new();
    let Ok(outs) = out_handles(cmd, args) else { return Step::Done(fail(VK_ERROR_INITIALIZATION_FAILED)) };
    for (kind, ptr, n) in outs {
        for i in 0..n {
            let id = st.alloc_id();
            std::ptr::write_unaligned(ptr.add(i), id);
            if DISPATCHABLE.contains(&kind) {
                disp_outs.push((ptr.add(i), id));
            }
        }
    }
    if cmd.name.starts_with("vkQueueSubmit") {
        // vkd3d-proton zgłasza ok. 8 razy na klatkę, a skan 25 pamięci kosztuje ok. 70 µs każda. Aplikacja nie może legalnie zmienić pamięci
        // czytanej przez zgłoszoną pracę przed oczekiwaniem (a to jest punkt synchronizacji), więc skan odkładamy do wysłania partii.
        if flags().eager_scan {
            st.write_mapped();
        } else {
            st.scan_pending = true;
        }
        st.gpu_dirty = true;
        st.submit_gen += 1;
    }
    let mut e = Encoder::new();
    if let Err(err) = encode_command(op, args, &mut e, &IcdCtx) {
        // Dotąd cichy błąd: polecenie znikało bez śladu (tak "zniknęły" deskryptory z `vkGetDescriptorEXT`). Raz na nazwę polecenia.
        static REPORTED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
        let mut seen = REPORTED.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.contains(&cmd.name) {
            seen.push(cmd.name);
            eprintln!("gpu-icd: nie umiem zakodować {}: {err:?}", cmd.name);
        }
        return Step::Done(fail(VK_ERROR_INITIALIZATION_FAILED));
    }
    let bytes = e.finish();
    // `vkGetDescriptorEXT` (VK_EXT_descriptor_buffer): bajty deskryptora to funkcja zakodowanego żądania, więc powtórzenia idą z cache.
    let desc_key = (cmd.name == "vkGetDescriptorEXT" && slots.len() >= 4 && slots[3] != 0 && slots[2] <= DESC_CACHE_MAX_BYTES).then(|| bytes.clone());
    if let Some(key) = &desc_key {
        if desc_cache_hit(key, slots[3], slots[2]) {
            st.stats.desc_hits += 1;
            return Step::Done(0);
        }
        st.stats.desc_misses += 1;
        // VkDescriptorGetInfoEXT: `type` pod offsetem 16.
        let ty = if slots[1] != 0 { std::ptr::read_unaligned((slots[1] as *const u8).add(16) as *const u32) } else { u32::MAX };
        st.stats.desc_miss_by_type[(ty as usize).min(11)] += 1;
    }
    if cmd.name == "vkAllocateMemory" {
        if let Some(size) = gpu_vkcodec::codec::arg_uint(cmd, args, "pAllocateInfo->allocationSize") {
            let id = disp_outs.first().map(|x| x.1).unwrap_or(0);
            let mem_id = std::ptr::read_unaligned(slots[3] as *const u64);
            let _ = id;
            st.mem_sizes.insert(mem_id, size);
            let known = memory_is_cached(st, slots, cmd, args);
            let cached = known.unwrap_or(true);
            st.mem_cached.insert(mem_id, known.unwrap_or(false));
            st.stats.mem_flags.insert(mem_id, (size, LAST_MEMORY_FLAGS.load(std::sync::atomic::Ordering::Relaxed)));
            let refresh = if flags().no_refresh_uncached { cached } else { size <= REFRESH_ALWAYS_BELOW || cached };
            st.mem_refresh.insert(mem_id, refresh);
        }
    }
    if (cmd.name == "vkCreateImage" || cmd.name == "vkCreateBuffer") && slots[1] != 0 && slots[3] != 0 {
        let id = std::ptr::read_unaligned(slots[3] as *const u64);
        let key = if cmd.name == "vkCreateImage" { crate::memreq::MemReqCache::image_key(slots[1] as *const u8) } else { crate::memreq::MemReqCache::buffer_key(slots[1] as *const u8) };
        st.memreq.object_created(id, key);
    }
    if (cmd.name == "vkDestroyImage" || cmd.name == "vkDestroyBuffer") && slots[1] != 0 {
        st.memreq.object_destroyed(slots[1]);
        st.gpuw.buffer_destroyed(slots[1]);
    }
    if cmd.name == "vkCreateBuffer" && slots[1] != 0 && slots[3] != 0 {
        let id = std::ptr::read_unaligned(slots[3] as *const u64);
        let has_pnext = std::ptr::read_unaligned((slots[1] as *const u8).add(8) as *const u64) != 0;
        let usage = if has_pnext { None } else { gpu_vkcodec::codec::arg_uint(cmd, args, "pCreateInfo->usage") };
        let size = gpu_vkcodec::codec::arg_uint(cmd, args, "pCreateInfo->size").unwrap_or(0);
        st.gpuw.buffer_created(id, size, usage.map_or(u32::MAX, |u| u as u32));
    }
    note_memory_binding(st, cmd.name, slots);
    if cmd.name == "vkCreateDescriptorSetLayout" && slots[1] != 0 && slots[3] != 0 {
        st.desc.layout_created(std::ptr::read_unaligned(slots[3] as *const u64), slots[1] as *const u8);
    }
    if cmd.name == "vkCreateDescriptorPool" && slots[1] != 0 && slots[3] != 0 {
        st.desc.pool_created(std::ptr::read_unaligned(slots[3] as *const u64), slots[1] as *const u8);
    }
    if cmd.name == "vkCreateDescriptorUpdateTemplate" && slots[1] != 0 && slots[3] != 0 {
        // Identyfikator szablonu został już wpisany w pamięć aplikacji; zapamiętujemy jego wpisy.
        let id = std::ptr::read_unaligned(slots[3] as *const u64);
        st.templates.insert(id, crate::templates::parse_entries(slots[1] as *const u8));
    }
    if cmd.name == "vkDestroyDescriptorUpdateTemplate" {
        st.templates.remove(&slots[1]);
    }
    if cmd.name == "vkCreateDevice" {
        if let (Some((_, dev_id)), true) = (disp_outs.first(), slots[0] != 0) {
            // PdevObj: identyfikator zdalnej karty pod offsetem 8.
            st.device_pdev.insert(*dev_id, std::ptr::read_unaligned((slots[0] as *const u8).add(8) as *const u64));
        }
    }
    // Obiekty dyspozycyjne: aplikacja dostaje wskaźnik na obiekt ICD, a w protokole obowiązuje jego identyfikator.
    for (slot, id) in disp_outs {
        let obj = Box::into_raw(Box::new(DispObj::new(id)));
        std::ptr::write_unaligned(slot, obj as u64);
        if cmd.name == "vkGetDeviceQueue" {
            let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
            st.queues.insert((dev, slots[1] as u32, slots[2] as u32), obj as usize);
        }
    }
    // Zwolnienie obiektów ICD po zniszczeniu (identyfikator został już zakodowany).
    if cmd.name == "vkDestroyDevice" && slots[0] != 0 {
        drop(Box::from_raw(slots[0] as *mut DispObj));
    }
    if cmd.name == "vkFreeCommandBuffers" {
        for i in 0..slots[2] as usize {
            let p = std::ptr::read_unaligned((slots[3] as *const u64).add(i));
            if p != 0 {
                drop(Box::from_raw(p as *mut DispObj));
            }
        }
    }
    if cmd.name == "vkFreeMemory" {
        st.mems.remove(&slots[1]);
        st.mem_sizes.remove(&slots[1]);
        st.mem_refresh.remove(&slots[1]);
        st.mem_cached.remove(&slots[1]);
        st.gpuw.memory_freed(slots[1]);
    }

    st.stats.commands += 1;
    let has_data_out = params.iter().any(|p| p.shape == Shape::Pointer && !p.is_const && p.elem != Elem::Handle && p.ptr_depth == 1);
    if force_async || (cmd.class != CmdClass::Sync && !has_data_out && cmd.ret != Ret::U64 && !is_pipeline_probe(cmd.name, slots) && !(may_fail_legitimately(cmd.name) && !speculated)) {
        st.batch.extend(bytes);
        if st.batch.len() > BATCH_FLUSH {
            st.flush();
        }
        return Step::Done(0);
    }
    // Droga synchroniczna: partia, potem polecenie, potem `Sync` (błąd asynchroniczny); jeden obieg po sieci.
    if st.scan_pending {
        st.settle_memory();
    }
    st.stats.query_bytes += bytes.len() as u64;
    st.stats.roundtrips += 1;
    // Po oczekiwaniu trzeba odświeżyć odwzorowane pamięci: żądania wysyłamy zaraz za `Sync`, w tym samym obiegu po sieci.
    let pre_ids: Vec<u64> = if cmd.class == CmdClass::Sync && st.gpu_dirty { st.refresh_ids() } else { Vec::new() };
    // Wszystko (zaległa partia poleceń, zapytanie, Sync i odświeżenia) idzie jednym zapisem do gniazda, czyli jednym pakietem.
    let mut reqs: Vec<Request> = Vec::with_capacity(3 + pre_ids.len());
    if !st.batch.is_empty() {
        let data = std::mem::take(&mut st.batch);
        st.stats.cmd_bytes += data.len() as u64;
        st.stats.cmd_batches += 1;
        reqs.push(Request::Cmds { data });
    }
    reqs.push(Request::Query { data: bytes });
    reqs.push(Request::Sync);
    for id in &pre_ids {
        let req = st.delta_request(*id);
        reqs.push(req);
    }
    let ticket = st.client.send_many(&reqs).ok();
    Step::Pending(Pending {
        op,
        slots_ptr: slots.as_mut_ptr(),
        slots_len: slots.len(),
        pre_ids,
        alloc_ctx,
        speculated,
        audited,
        desc_key,
        base: ticket.unwrap_or(0),
        sent_ok: ticket.is_some(),
        t_recv: std::time::Instant::now(),
        submit_gen: st.submit_gen,
        shared: st.client.shared(),
    })
}


/// Stan wątku, który wysłał żądanie z odpowiedzią i czeka na nią BEZ blokady instancji (patrz `link`).
pub(crate) struct Pending {
    op: u16,
    slots_ptr: *mut u64,
    slots_len: usize,
    pre_ids: Vec<u64>,
    alloc_ctx: Option<(u64, Vec<u64>)>,
    speculated: bool,
    audited: bool,
    desc_key: Option<Vec<u8>>,
    base: u64,
    sent_ok: bool,
    t_recv: std::time::Instant,
    submit_gen: u64,
    shared: std::sync::Arc<crate::link::Shared>,
}

/// Wynik pierwszej fazy wykonania polecenia: gotowa wartość albo oczekiwanie na odpowiedź serwera.
pub(crate) enum Step {
    Done(u64),
    Pending(Pending),
}

/// Odpowiedzi na żądania z `Pending`: zapytanie, `Sync` i odświeżenia pamięci (po jednej na każde).
pub(crate) struct Responses {
    q: Result<Response, gpu_renderd::client::ClientError>,
    sy: Result<Response, gpu_renderd::client::ClientError>,
    deltas: Vec<Option<Vec<u8>>>,
}

impl Pending {
    /// Odbiera odpowiedzi w kolejności biletów; nie wymaga blokady instancji (to cały sens: inne wątki gry pracują w tym czasie).
    pub(crate) fn receive(&self) -> Responses {
        if !self.sent_ok {
            let closed = || Err(gpu_renderd::client::ClientError::Closed);
            return Responses { q: closed(), sy: closed(), deltas: vec![None; self.pre_ids.len()] };
        }
        let q = self.shared.recv_at(self.base);
        let sy = self.shared.recv_at(self.base + 1);
        // Odpowiedzi na odświeżenia przychodzą zawsze, więc czytamy je od razu (także przy późniejszym błędzie), żeby nie rozsynchronizować strumienia.
        let deltas = (0..self.pre_ids.len()).map(|i| if let Ok(Response::Data(d)) = self.shared.recv_at(self.base + 2 + i as u64) { Some(d) } else { None }).collect();
        Responses { q, sy, deltas }
    }
}

/// Wykonuje polecenie w całości pod blokadą instancji (wołający już ją ma): pierwsza faza, odbiór odpowiedzi, druga faza.
///
/// # Safety
/// Jak `exec`.
pub(crate) unsafe fn exec_with(st: &mut InstState, op: u16, slots: &mut [u64], force_async: bool) -> u64 {
    match exec_begin(st, op, slots, force_async) {
        Step::Done(v) => v,
        Step::Pending(p) => {
            let r = p.receive();
            exec_finish(st, p, r)
        }
    }
}

/// Druga faza polecenia synchronicznego: obróbka odpowiedzi serwera pod blokadą instancji.
///
/// # Safety
/// `p` pochodzi z `exec_begin` tego samego wywołania, a bufor `slots` (argumenty aplikacji) nadal żyje.
pub(crate) unsafe fn exec_finish(st: &mut InstState, p: Pending, r: Responses) -> u64 {
    let Some(cmd) = COMMANDS.get(p.op as usize) else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    let slots: &mut [u64] = std::slice::from_raw_parts_mut(p.slots_ptr, p.slots_len);
    let args = slots.as_ptr() as *const u8;
    let fail = |e: i32| -> u64 { e as u32 as u64 };
    let (speculated, audited, desc_key, alloc_ctx) = (p.speculated, p.audited, p.desc_key.clone(), p.alloc_ctx.clone());
    // Odpowiedzi na odświeżenia przyszły razem z resztą (czytane bez blokady), więc tu tylko je wpisujemy.
    let t_refresh = std::time::Instant::now();
    let mut refreshed = false;
    for (i, (id, d)) in p.pre_ids.iter().zip(r.deltas).enumerate() {
        if let Some(d) = d {
            if st.apply_delta(*id, &d, p.base + 2 + i as u64) {
                st.drain_delta(*id);
            }
            refreshed = true;
        }
    }
    if refreshed {
        st.stats.refreshes += 1;
        st.stats.t_refresh_ns += t_refresh.elapsed().as_nanos() as u64;
    }
    st.stats.t_syncrecv_ns += p.t_recv.elapsed().as_nanos() as u64;
    let e = st.stats.rt_by_cmd.entry(cmd.name).or_insert((0, 0));
    e.0 += 1;
    e.1 += p.t_recv.elapsed().as_nanos() as u64;
    if let Ok(Response::Synced { error }) = r.sy {
        if error < 0 {
            st.lose();
        }
    }
    let Ok(Response::Data(d)) = r.q else {
        st.lose();
        return fail(VK_ERROR_DEVICE_LOST);
    };
    let result = d.get(..4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(VK_ERROR_DEVICE_LOST);
    let mut dec = Decoder::new(d.get(4..).unwrap_or(&[]));
    if result < 0 {
        // Raz na parę (polecenie, kod): serwer odmówił, a aplikacja widzi tylko kod wyniku (tak "Failed to create Vulkan device" nie miało przyczyny w logu).
        static SEEN: std::sync::Mutex<Vec<(&'static str, i32)>> = std::sync::Mutex::new(Vec::new());
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.contains(&(cmd.name, result)) && seen.len() < 64 {
            seen.push((cmd.name, result));
            eprintln!("gpu-icd: serwer odpowiedział na {} kodem {result}", cmd.name);
        }
    }
    if result >= 0 && decode_out_params(cmd, args, &mut dec).is_err() {
        eprintln!("gpu-icd: nie umiem odczytać odpowiedzi na {}", cmd.name);
        return fail(VK_ERROR_INITIALIZATION_FAILED);
    }
    if cmd.ret == Ret::U64 {
        // Wartość zwracana (np. adres bufora) idzie na końcu odpowiedzi; brak = 0, czyli adres nieprawidłowy.
        return if result >= 0 { dec.get_u64().unwrap_or(0) } else { 0 };
    }
    if result >= 0 {
        memreq_store(st, cmd.name, slots);
        devreq_store(st, cmd.name, slots);
        if cmd.name == "vkGetDescriptorEXT" && slots.len() >= 4 && slots[3] != 0 && slots[2] <= DESC_CACHE_MAX_BYTES {
            if let Some((ty, address, range)) = crate::descsynth::input_of(slots[1] as *const u8) {
                let actual = std::slice::from_raw_parts(slots[3] as *const u8, slots[2] as usize);
                match st.synth.observe(ty, address, range, actual) {
                    crate::descsynth::Event::Verified => eprintln!("gpu-icd: deskryptory buforów typu {ty} liczone lokalnie (wzór potwierdzony {} odpowiedziami serwera)", crate::descsynth::LEARN_NEEDED),
                    crate::descsynth::Event::Disabled => eprintln!("gpu-icd: wzór deskryptora typu {ty} NIE zgadza się z serwerem (adres {address:#x}, zakres {range:#x}); liczenie lokalne wyłączone"),
                    crate::descsynth::Event::None => {}
                }
            }
        }
        if let Some(key) = desc_key {
            desc_cache_store(key, std::slice::from_raw_parts(slots[3] as *const u8, slots[2] as usize).to_vec());
        }
    }
    if let Some((pool, layouts)) = &alloc_ctx {
        if !speculated {
            // Seria odmów z rzędu (bez żadnej udanej alokacji między nimi) to moment, w którym gra się wywala; normalny cykl to jedna odmowa
            // i reset puli. Opisujemy od trzeciej odmowy z rzędu, także dla puli nieznanej księgowości.
            static REFUSED_RUN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            static EXPLAINED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if result < 0 {
                let run = REFUSED_RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if run >= 3 && EXPLAINED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 24 {
                    eprintln!("[gpu-icd] odmowa alokacji zestawów nr {run} z rzędu ({result}): {}", st.desc.explain(*pool, layouts));
                }
            } else {
                REFUSED_RUN.store(0, std::sync::atomic::Ordering::Relaxed);
            }
            if audited && result < 0 {
                DESC_SPEC_BROKEN.store(true, std::sync::atomic::Ordering::Relaxed);
                eprintln!("[gpu-icd] audyt alokacji zestawów: serwer odmówił ({result}), choć księgowość przewidziała sukces; spekulacja WYŁĄCZONA. {}", st.desc.explain(*pool, layouts));
            }
            st.desc.after_sync(*pool, layouts, result >= 0);
        }
    }
    if result < 0 {
        // Nieudana alokacja: uchwyty wyjściowe mają być VK_NULL_HANDLE, a rozmiar pamięci nie ma być zapamiętany.
        if cmd.name == "vkAllocateDescriptorSets" && slots[1] != 0 && slots[2] != 0 {
            let n = std::ptr::read_unaligned((slots[1] as *const u8).add(24) as *const u32).min(65536) as usize;
            for i in 0..n {
                std::ptr::write_unaligned((slots[2] as *mut u64).add(i), 0);
            }
        }
        if cmd.name == "vkAllocateMemory" && slots[3] != 0 {
            let id = std::ptr::read_unaligned(slots[3] as *const u64);
            st.mem_sizes.remove(&id);
            st.mem_refresh.remove(&id);
            st.mem_cached.remove(&id);
            std::ptr::write_unaligned(slots[3] as *mut u64, 0);
        }
    }
    if cmd.class == CmdClass::Sync && result == 0 {
        if st.gpu_dirty {
            st.lazy_invalidate();
        }
        if refreshed {
            // Pamięci zostały odświeżone w tym samym obiegu co oczekiwanie. Jeśli w międzyczasie (gdy czekaliśmy bez blokady) inny wątek
            // zgłosił pracę, jego zapisy GPU nie były objęte tym odświeżeniem, więc stan "brudny" zostaje.
            if st.submit_gen == p.submit_gen {
                st.gpu_dirty = false;
            }
        } else {
            st.read_mapped();
        }
    }
    fail(result)
}

/// Pobranie zakresu leniwej pamięci z serwera dla wątku obsługi błędów stron (`lazymap`). Blokada instancji z limitem czasu: gdy wątek
/// aplikacji sam ją trzyma (przekazał nam wskaźnik do odwzorowanej pamięci), oddajemy stronę bez pobrania zamiast zakleszczyć grę.
fn lazy_fetch(r: &crate::lazymap::LazyRegion, off: usize, len: usize) -> bool {
    let Some(inst) = state() else { return false };
    crate::watchdog::set(0, crate::watchdog::LAZY_FETCH, off as u64);
    let _wd = WdGuard;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut st = loop {
        match inst.state.try_lock() {
            Ok(g) => break g,
            Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    crate::lazymap::FETCH_TIMEOUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
        }
    };
    st.read_mem_into_shadow(r.memory, off as u64, len)
}

/// Opis stanu połączenia do raportu watchdoga (bez blokowania: gdy blokada instancji jest zajęta, mówi o tym).
pub(crate) fn link_state() -> String {
    match state() {
        Some(inst) => match inst.state.try_lock() {
            Ok(st) => st.client.describe(),
            Err(_) => "blokada instancji zajęta (stanu połączenia nie da się odczytać bez niej)".to_string(),
        },
        None => "brak instancji".to_string(),
    }
}

pub(crate) fn fast_commands() -> u64 {
    FAST_COMMANDS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Kończy wpis watchdoga bieżącego wątku przy każdym wyjściu z `forward`.
pub(crate) struct WdGuard;

impl Drop for WdGuard {
    fn drop(&mut self) {
        crate::watchdog::done();
    }
}

pub(crate) fn install_lazy() {
    if flags().lazy {
        crate::lazymap::set_fetch(lazy_fetch);
    }
}

// ---------- pamięć odwzorowana (lokalnie; synchronizacja z serwerem w punktach) ----------

pub unsafe extern "C" fn map_memory(_dev: u64, mem: u64, offset: u64, size: u64, _flags: u32, out: *mut *mut u8) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut st = lock_state(inst);
    let Some(total) = st.mem_sizes.get(&mem).copied() else { return VK_ERROR_MEMORY_MAP_FAILED };
    if offset > total || (size != u64::MAX && offset.saturating_add(size) > total) || out.is_null() {
        return VK_ERROR_MEMORY_MAP_FAILED;
    }
    if !st.mems.contains_key(&mem) {
        // Każda pamięć, która po oczekiwaniu byłaby odświeżana z serwera (HOST_CACHED albo mała), ma odczyt leniwy; pamięci bez odświeżania (duże, bez cache) nie.
        // Pamięć bez HOST_CACHED dostaje odczyt leniwy tylko na życzenie (`GPU_ICD_LAZY_ALL=1`): w Black Book (DXVK) zawieszał GPU serwera.
        let cached = st.stats.mem_flags.get(&mem).is_some_and(|&(_, fl)| fl & 0x8 != 0);
        let eligible = flags().lazy && (cached || lazy_all()) && st.mem_refresh.get(&mem).copied().unwrap_or(false) && (LAZY_MIN..=LAZY_MAX).contains(&(total as usize));
        let lazy = if eligible { Shadow::new_lazy(total as usize, mem) } else { None };
        if lazy.is_some() {
            st.stats.lazy_memories += 1;
            if flags().periodic || flags().debug {
                eprintln!("[gpu-icd] odczyt leniwy włączony dla pamięci {mem:#x} ({} KB)", total / 1024);
            }
        }
        let sh = lazy.unwrap_or_else(|| Shadow::new(total as usize));
        st.mems.insert(mem, sh);
    }
    let Some(sh) = st.mems.get_mut(&mem) else { return VK_ERROR_MEMORY_MAP_FAILED };
    sh.mapped = true;
    if let Some(r) = &sh.lazy {
        // Po ponownym odwzorowaniu zawartość mogła się zmienić po stronie GPU.
        r.invalidate(&[(0, r.len() as u64)]);
    }
    *out = sh.app_ptr().add(offset as usize);
    0
}

pub unsafe extern "C" fn unmap_memory(_dev: u64, mem: u64) {
    if let Some(inst) = state() {
        let mut st = lock_state(inst);
        let span = st.mems.get_mut(&mem).map(|s| {
            s.mapped = false;
            (s.data.as_ptr(), s.dirty_ranges())
        });
        // Zapis z odwzorowania musi dotrzeć na serwer, bo po Unmap nie będzie już śledzony.
        if let Some((p, ranges)) = span {
            for (a, b) in ranges {
                st.send_mem(mem, a as u64, std::slice::from_raw_parts(p.add(a), b - a));
            }
        }
    }
}

unsafe fn ranges(count: u32, r: *const u8) -> Vec<(u64, u64, u64)> {
    // VkMappedMemoryRange (40 B): memory @16, offset @24, size @32.
    (0..count as usize)
        .map(|i| {
            let b = r.add(i * 40);
            (std::ptr::read_unaligned(b.add(16) as *const u64), std::ptr::read_unaligned(b.add(24) as *const u64), std::ptr::read_unaligned(b.add(32) as *const u64))
        })
        .collect()
}

pub unsafe extern "C" fn flush_ranges(_dev: u64, count: u32, r: *const u8) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut st = lock_state(inst);
    for (mem, off, size) in ranges(count, r) {
        let Some(sh) = st.mems.get(&mem) else { continue };
        let end = if size == u64::MAX { sh.data.len() as u64 } else { off.saturating_add(size).min(sh.data.len() as u64) };
        let part = sh.data.get(off as usize..end as usize).map(|s| s.to_vec()).unwrap_or_default();
        st.send_mem(mem, off, &part);
        if let Some(sh) = st.mems.get_mut(&mem) {
            sh.mark_synced(off as usize, end as usize);
        }
    }
    0
}

pub unsafe extern "C" fn invalidate_ranges(_dev: u64, count: u32, r: *const u8) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut st = lock_state(inst);
    let mut map: HashMap<u64, ()> = HashMap::new();
    for (mem, off, size) in ranges(count, r) {
        let Some(len) = st.mems.get(&mem).map(|s| s.data.len() as u64) else { continue };
        let end = if size == u64::MAX { len } else { off.saturating_add(size).min(len) };
        if st.read_mem_into_shadow(mem, off, (end - off) as usize) {
            if let Some(r) = st.mems.get(&mem).and_then(|s| s.lazy.as_ref()) {
                r.mark_fresh(off as usize, end as usize);
            }
        }
        map.insert(mem, ());
    }
    0
}

/// Wykonuje polecenie po nazwie (dla kodu ICD wydającego własne polecenia, np. swapchain). Zwraca kod wyniku.
///
/// # Safety
/// Jak `exec`.
pub(crate) unsafe fn exec_named(st: &mut InstState, name: &str, slots: &mut [u64]) -> i32 {
    match gpu_proto::meta::find_command(name) {
        Some((op, c)) if c.params().len() == slots.len() => exec(st, op, slots) as u32 as i32,
        _ => VK_ERROR_INITIALIZATION_FAILED,
    }
}

impl InstState {
    /// Właściwości pamięci zdalnej karty (`MemoryProperties2`, z pamięci podręcznej).
    pub(crate) fn query_props(&mut self, pdev: u64) -> Option<Vec<u8>> {
        self.query(pdev, gpu_proto::msg::PdFunc::MemoryProperties2, 0, 0, &[])
    }
}

/// Jak `exec_named`, ale bez czekania na wynik (polecenie trafia do partii).
///
/// # Safety
/// Jak `exec`.
pub(crate) unsafe fn exec_named_async(st: &mut InstState, name: &str, slots: &mut [u64]) -> i32 {
    match gpu_proto::meta::find_command(name) {
        Some((op, c)) if c.params().len() == slots.len() => exec_with(st, op, slots, true) as u32 as i32,
        _ => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[cfg(test)]
mod pipeline_probe_tests {
    use super::*;

    #[test]
    fn only_infos_with_the_fail_on_compile_required_flag_force_a_synchronous_create() {
        // Dwie struktury VkGraphicsPipelineCreateInfo (144 B): flagi @16.
        let mut mem = vec![0u8; 144 * 2];
        let slots = |m: &Vec<u8>| vec![1u64, 0, 2, m.as_ptr() as u64, 0, 0];
        unsafe {
            assert!(!is_pipeline_probe("vkCreateGraphicsPipelines", &slots(&mem)));
            mem[144 + 16] = 0x00;
            mem[144 + 17] = 0x01; // 0x100 w drugiej strukturze
            assert!(is_pipeline_probe("vkCreateGraphicsPipelines", &slots(&mem)));
            assert!(!is_pipeline_probe("vkCreateBuffer", &slots(&mem)));
            // Zerowy wskaźnik i absurdalna liczba nie powodują odczytu.
            assert!(!is_pipeline_probe("vkCreateGraphicsPipelines", &[1, 0, 2, 0]));
            assert!(!is_pipeline_probe("vkCreateGraphicsPipelines", &[1, 0, 1 << 40, mem.as_ptr() as u64]));
        }
    }
}

#[cfg(test)]
mod alloc_tests {
    use super::*;

    #[test]
    fn only_allocations_that_may_legitimately_fail_are_synchronous() {
        assert!(may_fail_legitimately("vkAllocateDescriptorSets"));
        assert!(may_fail_legitimately("vkAllocateMemory"));
        assert!(!may_fail_legitimately("vkAllocateCommandBuffers"));
        assert!(!may_fail_legitimately("vkCreateBuffer"));
    }
}

#[cfg(test)]
mod shadow_tests {
    use super::*;
    use gpu_proto::pagediff::PAGE;

    #[test]
    fn large_shadow_reports_only_written_pages_and_ignores_rewrites_of_the_same_bytes() {
        let mut s = Shadow::new(4 * 1024 * 1024 + 100);
        assert!(s.dirty_ranges().is_empty());
        s.data[PAGE * 10 + 1] = 7;
        s.data[PAGE * 11] = 8;
        s.data[4 * 1024 * 1024 + 50] = 9; // niepełna ostatnia strona
        assert_eq!(s.dirty_ranges(), vec![(PAGE * 10, PAGE * 12), (4 * 1024 * 1024, 4 * 1024 * 1024 + 100)]);
        assert!(s.dirty_ranges().is_empty());
        s.data[PAGE * 10 + 1] = 7; // zapis tej samej wartości: strona zapisana, treść bez zmian
        assert!(s.dirty_ranges().is_empty());
        s.data[PAGE * 10 + 1] = 1;
        assert_eq!(s.dirty_ranges(), vec![(PAGE * 10, PAGE * 11)]);
    }

    #[test]
    fn data_copied_from_the_server_into_a_large_shadow_is_not_sent_back() {
        let mut s = Shadow::new(2 * 1024 * 1024);
        s.dirty_ranges();
        s.data[PAGE * 3..PAGE * 4].fill(0x5A);
        s.mark_synced(PAGE * 3, PAGE * 4);
        assert!(s.dirty_ranges().is_empty());
    }

    #[test]
    fn small_shadow_without_kernel_tracking_still_works() {
        let mut s = Shadow::new(PAGE * 8);
        s.data[PAGE * 2] = 1;
        assert_eq!(s.dirty_ranges(), vec![(PAGE * 2, PAGE * 3)]);
    }
}

#[cfg(test)]
mod memreq_target_tests {
    use super::*;

    #[test]
    #[allow(unused_assignments)] // wartości czytane przez surowe wskaźniki, których kompilator nie śledzi
    fn dedicated_requirements_chain_is_accepted_but_other_chains_and_info_pnext_are_not() {
        let mut info = [0u64; 3]; // VkImageMemoryRequirementsInfo2: obiekt @16
        info[2] = 77;
        let mut node = [0u64; 3]; // VkMemoryDedicatedRequirements
        node[0] = STYPE_MEMORY_DEDICATED_REQUIREMENTS as u64;
        let mut out = [0u64; 5]; // VkMemoryRequirements2
        out[1] = node.as_mut_ptr() as u64;
        let slots = [1u64, info.as_ptr() as u64, out.as_mut_ptr() as u64];
        unsafe {
            let (obj, _, ded) = memreq_target("vkGetImageMemoryRequirements2", &slots).unwrap();
            assert_eq!(obj, 77);
            assert_eq!(ded, Some(node.as_mut_ptr() as *mut u8));
            // Inny typ struktury w odpowiedzi => brak cache.
            node[0] = 1_000_000_000;
            assert!(memreq_target("vkGetImageMemoryRequirements2", &slots).is_none());
            // Dalszy element w łańcuchu => brak cache.
            node[0] = STYPE_MEMORY_DEDICATED_REQUIREMENTS as u64;
            node[1] = 0xABC;
            assert!(memreq_target("vkGetImageMemoryRequirements2", &slots).is_none());
            node[1] = 0;
            // pNext w pInfo => brak cache.
            info[1] = 0x1234;
            assert!(memreq_target("vkGetImageMemoryRequirements2", &slots).is_none());
            // Wariant bez „2" i obcy command.
            let s1 = [1u64, 55, out.as_mut_ptr() as u64];
            assert_eq!(memreq_target("vkGetBufferMemoryRequirements", &s1).unwrap().0, 55);
            assert!(memreq_target("vkCreateBuffer", &s1).is_none());
        }
    }

    #[test]
    fn only_plain_command_recording_takes_the_lock_free_fast_path() {
        let idx = |name: &str| gpu_proto::meta::find_command(name).map(|(op, _)| op as usize).unwrap();
        let t = fast_table();
        for yes in ["vkCmdDraw", "vkCmdDrawIndexed", "vkCmdDispatch", "vkCmdBindPipeline", "vkCmdSetViewportWithCount", "vkCmdPipelineBarrier2", "vkCmdPushConstants"] {
            assert!(t[idx(yes)], "{yes} powinno iść szybką ścieżką");
        }
        // Wszystko, co zwraca wynik, ma obsługę w `exec` albo potrzebuje kolejności względem blokady: zwykła ścieżka.
        for no in ["vkEndCommandBuffer", "vkBeginCommandBuffer", "vkQueueSubmit", "vkQueueSubmit2", "vkCreateBuffer", "vkGetDescriptorEXT", "vkAllocateMemory", "vkWaitForFences", "vkResetCommandPool", "vkUpdateDescriptorSets"] {
            assert!(!t[idx(no)], "{no} nie może iść szybką ścieżką");
        }
        // Żadne polecenie szybkiej ścieżki nie ma specjalnej obsługi po nazwie w `exec` (te zaczynają się od innych przedrostków).
        for (op, c) in COMMANDS.iter().enumerate() {
            if t[op] {
                assert!(c.name.starts_with("vkCmd"), "{}", c.name);
            }
        }
    }

    #[test]
    fn commands_of_one_command_buffer_keep_their_order_when_recorded_on_two_threads() {
        // vkd3d-proton: wątek A nagrywa część bufora, wątek B resztę i zamyka go; polecenia A nie mogą wyprzedzić End z wątku B.
        let cb = std::sync::Arc::new(DispObj::new(7));
        let a = { let cb = cb.clone(); std::thread::spawn(move || { cb.push(&[1, 2]); cb.push(&[3]); }) };
        a.join().unwrap();
        let b = { let cb = cb.clone(); std::thread::spawn(move || { cb.push(&[4]); cb.take() }) };
        assert_eq!(b.join().unwrap(), vec![1, 2, 3, 4], "polecenia z obu wątków w kolejności nagrywania");
        assert!(cb.take().is_empty());
        // Bufory różnych buforów poleceń są niezależne; próg opróżnienia.
        let other = DispObj::new(8);
        assert!(!other.push(&[9]));
        assert!(other.push(&vec![0u8; LOCAL_FLUSH_BYTES]));
        assert!(cb.take().is_empty());
    }

    #[test]
    fn lazy_shadow_tracks_writes_made_through_the_app_view() {
        const PAGE: usize = 4096;
        let Some(mut s) = Shadow::new_lazy(PAGE * 64, 77) else {
            eprintln!("POMINIĘTO: brak userfaultfd MINOR dla shmem");
            return;
        };
        assert!(s.dirty_ranges().is_empty());
        // SAFETY: zapisy w granicach widoku aplikacji.
        unsafe {
            *s.app_ptr().add(PAGE * 3 + 5) = 9;
            *s.app_ptr().add(PAGE * 10) = 1;
        }
        assert_eq!(s.dirty_ranges(), vec![(PAGE * 3, PAGE * 4), (PAGE * 10, PAGE * 11)]);
        assert_eq!(s.data[PAGE * 3 + 5], 9, "widok własny widzi zapis aplikacji");
        assert!(s.dirty_ranges().is_empty(), "po odczycie stan jest czysty");
    }

    #[test]
    fn an_older_response_never_overwrites_a_newer_one_on_the_same_page() {
        const PAGE: usize = 4096;
        let mut s = Shadow::new(PAGE * 4);
        // Nowsza odpowiedź (bilet 7) wpisała stronę 1, potem przychodzi starsza (bilet 5): strona zostaje z nowszą treścią.
        s.apply_at(PAGE, &[7u8; PAGE], 7);
        s.apply_at(PAGE, &[5u8; PAGE], 5);
        assert!(s.data[PAGE..PAGE * 2].iter().all(|&b| b == 7), "starsza odpowiedź nie cofa strony");
        // Odpowiedź obejmująca strony 0-1: strona 0 (nikt jej nie wpisał) dostaje dane, strona 1 zostaje z nowszymi.
        s.apply_at(0, &[6u8; PAGE * 2], 6);
        assert!(s.data[..PAGE].iter().all(|&b| b == 6));
        assert!(s.data[PAGE..PAGE * 2].iter().all(|&b| b == 7));
        // Równy lub wyższy bilet nadpisuje; zakres niewyrównany do strony działa na części stron.
        s.apply_at(PAGE + 100, &[9u8; 50], 8);
        assert!(s.data[PAGE + 100..PAGE + 150].iter().all(|&b| b == 9));
        assert_eq!(s.data[PAGE + 99], 7);
        assert_eq!(s.data[PAGE + 150], 7);
        // Bilet 0 działa (wersja = bilet + 1): pierwsza odpowiedź w ogóle na świeżej stronie.
        s.apply_at(PAGE * 3, &[1u8; 16], 0);
        assert!(s.data[PAGE * 3..PAGE * 3 + 16].iter().all(|&b| b == 1));
    }

    #[test]
    fn unused_pointers_of_write_descriptor_sets_are_zeroed_per_descriptor_type() {
        let mk = |ty: u32| {
            let mut w = vec![0u8; 64];
            w[36..40].copy_from_slice(&ty.to_le_bytes());
            for (i, at) in [40usize, 48, 56].into_iter().enumerate() {
                w[at..at + 8].copy_from_slice(&(0xDEAD_0000u64 + i as u64).to_le_bytes());
            }
            w
        };
        let ptrs = |w: &[u8]| [40usize, 48, 56].map(|at| u64::from_le_bytes(w[at..at + 8].try_into().unwrap()) != 0);
        assert_eq!(ptrs(&sanitize_descriptor_writes(&mk(6))), [false, true, false], "UBO: tylko pBufferInfo");
        assert_eq!(ptrs(&sanitize_descriptor_writes(&mk(7))), [false, true, false], "SSBO");
        assert_eq!(ptrs(&sanitize_descriptor_writes(&mk(1))), [true, false, false], "obraz z samplerem: tylko pImageInfo");
        assert_eq!(ptrs(&sanitize_descriptor_writes(&mk(4))), [false, false, true], "widok bufora: tylko pTexelBufferView");
        assert_eq!(ptrs(&sanitize_descriptor_writes(&mk(1_000_138_000))), [true, true, true], "typ z rozszerzenia bez zmian");
        // Dwa zapisy w tablicy: każdy traktowany osobno.
        let mut two = mk(6);
        two.extend(mk(3));
        let out = sanitize_descriptor_writes(&two);
        assert_eq!((ptrs(&out[..64]), ptrs(&out[64..])), ([false, true, false], [true, false, false]));
    }

}
