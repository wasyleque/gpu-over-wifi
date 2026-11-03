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
    pub desc_hits: u64,
    pub desc_misses: u64,
    /// Chybienia cache deskryptorów według `VkDescriptorType` (0..=10; ostatnia pozycja: inne).
    pub desc_miss_by_type: [u64; 12],
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
    pub fn report(&self) -> String {
        let per = |v: u64| if self.frames == 0 { 0.0 } else { v as f64 / self.frames as f64 / 1e6 };
        format!(
            "gpu-icd statystyki: strony pamięci nowe/powtórzone {}/{}, LZ4 próbka {}→{} B; deskryptory (cache/serwer) {}/{}, chybienia wg typu {:?}; odświeżenia pamięci {} ({:.0} ms; całe {}, zakresy {} po średnio {} KB), oczekiwanie na odpowiedzi serwera {:.0} ms, skan pamięci {:.0} ms ({} MB), wysyłka różnic {:.0} ms; poleceń {}, partii {}, bajty poleceń {}, zapisy pamięci {} B (na łączu {} B), odczyty pamięci {} B, zapytania {} B, obiegi po sieci {}; klatek {}: na klatkę czekanie na GPU {:.1} ms, odczyt {:.1} ms, wyświetlenie lokalne {:.1} ms",
            self.pages_new, self.pages_dup, self.lz4_in, self.lz4_out, self.desc_hits, self.desc_misses, self.desc_miss_by_type, self.refreshes, self.t_refresh_ns as f64 / 1e6, self.refresh_whole, self.refresh_ranged, self.refresh_ranged_bytes / self.refresh_ranged.max(1) / 1024, self.t_syncrecv_ns as f64 / 1e6, self.t_scan_ns as f64 / 1e6, self.scan_bytes / 1_000_000, self.t_memsend_ns as f64 / 1e6, self.commands, self.cmd_batches, self.cmd_bytes, self.memw_bytes, self.memw_wire_bytes, self.memr_bytes, self.query_bytes, self.roundtrips, self.frames, per(self.t_wait_ns), per(self.t_read_ns), per(self.t_local_ns)
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
}

pub fn flags() -> &'static Flags {
    static F: std::sync::OnceLock<Flags> = std::sync::OnceLock::new();
    F.get_or_init(|| Flags {
        no_refresh_uncached: std::env::var_os("GPU_ICD_NO_REFRESH_UNCACHED").is_some(),
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
}

/// Od tego rozmiaru pamięć ma jądrowe śledzenie zapisów (mniejsze są tanie do przeskanowania w całości).
const TRACK_FROM: usize = 1024 * 1024;

impl Shadow {
    pub fn new(len: usize) -> Shadow {
        let data = crate::memtrack::PageAlignedBuf::new(len);
        let tracker = if len >= TRACK_FROM && std::env::var("GPU_ICD_NO_MEMTRACK").is_err() { crate::memtrack::Tracker::new(data.as_ptr(), data.capacity()) } else { None };
        Shadow { data, mapped: false, pages: gpu_proto::pagediff::PageTracker::new(len), tracker }
    }

    /// Zakresy zmienione przez aplikację od ostatniego wysłania. Z jądrowym śledzeniem sprawdzamy tylko zapisane strony
    /// (potwierdzone skrótem, więc strony zapisane bez zmiany treści nie idą do sieci); bez niego skanujemy całość.
    pub fn dirty_ranges(&mut self) -> Vec<(usize, usize)> {
        if let Some(t) = &self.tracker {
            match t.take_dirty(0, self.data.capacity()) {
                Some(cand) => return self.pages.changed_within(&self.data, &cand),
                None => self.tracker = None, // śledzenie przestało działać: wracamy do pełnego skanowania
            }
        }
        self.pages.changed(&self.data, usize::MAX).0
    }

    pub fn mark_synced(&mut self, from: usize, to: usize) {
        self.pages.mark_synced(&self.data, from, to);
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

    /// Przed `vkQueueSubmit`: serwer musi zobaczyć wszystko, co aplikacja wpisała w odwzorowaną pamięć.
    fn write_mapped(&mut self) {
        let ids: Vec<u64> = self.mems.iter().filter(|(_, s)| s.mapped).map(|(i, _)| *i).collect();
        for id in ids {
            let t0 = std::time::Instant::now();
            let Some((ranges, len)) = self.mems.get_mut(&id).map(|s| (s.dirty_ranges(), s.data.len())) else { continue };
            self.stats.t_scan_ns += t0.elapsed().as_nanos() as u64;
            self.stats.scan_bytes += len as u64;
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

    /// Po zakończonym oczekiwaniu (fence/idle) GPU mogło zapisać odwzorowaną pamięć: dociągamy z serwera tylko zmienione strony.
    /// Bez nowego `vkQueueSubmit` od ostatniego odświeżenia nic nie mogło się zmienić, więc nie pytamy.
    fn read_mapped(&mut self) {
        let t = std::time::Instant::now();
        self.read_mapped_inner();
        self.stats.t_refresh_ns += t.elapsed().as_nanos() as u64;
    }

    /// Odwzorowane pamięci, które po oczekiwaniu trzeba odświeżyć z serwera (mapowane i dozwolone do odczytu).
    fn refresh_ids(&self) -> Vec<u64> {
        self.mems
            .iter()
            .filter(|(i, s)| s.mapped && self.mem_refresh.get(i).copied().unwrap_or(true) && self.gpuw.plan(**i) != crate::gpuwrite::Plan::Skip)
            .map(|(i, _)| *i)
            .collect()
    }

    /// Wpisuje odpowiedź `MemReadDelta` do lokalnej kopii pamięci `id`; zwraca `true`, gdy serwer ma jeszcze zmiany do wysłania.
    fn apply_delta(&mut self, id: u64, d: &[u8]) -> bool {
        let Some((more, segs)) = gpu_proto::pagediff::decode_segments(d) else { return false };
        for (off, body) in segs {
            self.stats.memr_bytes += body.len() as u64;
            let (a, b) = (off as usize, off as usize + body.len());
            // Kopiujemy do istniejącego bufora: aplikacja trzyma wskaźnik z `vkMapMemory`.
            if let Some(s) = self.mems.get_mut(&id) {
                if b <= s.data.len() {
                    s.data[a..b].copy_from_slice(body);
                    s.mark_synced(a, b);
                }
            }
        }
        more
    }

    /// Żądanie odświeżenia pamięci `id`: tylko zakresy, które GPU może zapisać, albo cała pamięć, gdy nie wiadomo.
    fn delta_request(&mut self, id: u64) -> Request {
        let plan = self.gpuw.plan(id);
        let size = self.mem_sizes.get(&id).copied().unwrap_or(0);
        if flags().periodic && self.plans_logged.insert(id) {
            match &plan {
                crate::gpuwrite::Plan::Ranges(r) => eprintln!("[gpu-icd] odświeżanie pamięci {id:#x} ({} MB): {} zakresów, razem {} KB", size >> 20, r.len(), r.iter().map(|x| x.1).sum::<u64>() >> 10),
                p => eprintln!("[gpu-icd] odświeżanie pamięci {id:#x} ({} MB): {p:?}", size >> 20),
            }
        }
        match plan {
            crate::gpuwrite::Plan::Ranges(ranges) => {
                self.stats.refresh_ranged += 1;
                self.stats.refresh_ranged_bytes += ranges.iter().map(|r| r.1).sum::<u64>();
                Request::MemReadDeltaRanges { memory: id, ranges }
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
            let Ok(Response::Data(d)) = self.client.call(&req) else { return };
            if !self.apply_delta(id, &d) {
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

unsafe fn forward(op: u16, ints: [u64; 14], floats: [f64; 8]) -> u64 {
    let Some(cmd) = COMMANDS.get(op as usize) else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    let mut st = lock(&inst.state);
    let fail = |e: i32| -> u64 { e as u32 as u64 };
    if st.device_lost && cmd.ret == Ret::VkResult {
        return fail(VK_ERROR_DEVICE_LOST);
    }
    // Argumenty -> sloty (odwrotność thunka serwera).
    let params = cmd.params();
    let mut slots = vec![0u64; params.len()];
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
    if cmd.name == "vkCreateGraphicsPipelines" && flags().trace {
        for i in 0..slots.get(2).copied().unwrap_or(0).min(8) as usize {
            let info = (slots[3] as *const u8).add(i * 144);
            trace_text(&format!("  pipeline[{i}] app: {}", gpu_proto::debug::describe_graphics_pipeline_info(info)));
        }
    }
    let t0 = std::time::Instant::now();
    let r = exec(&mut st, op, &mut slots);
    trace(cmd.name, r, t0.elapsed().as_micros());
    periodic_stats(&st.stats);
    r
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
unsafe fn is_cached_type(st: &mut InstState, slots: &[u64], cmd: &gpu_proto::meta::CommandMeta, args: *const u8) -> bool {
    let Some(idx) = gpu_vkcodec::codec::arg_uint(cmd, args, "pAllocateInfo->memoryTypeIndex") else { return true };
    let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
    let Some(pdev) = st.device_pdev.get(&dev).copied() else { return true };
    let Some(props) = st.query_props(pdev) else { return true };
    let at = 20 + idx as usize * 8;
    // Nieznany typ: bezpieczniej odświeżać (poprawność przed szybkością).
    props.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x8 != 0).unwrap_or(true)
}

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
pub(crate) unsafe fn exec_with(st: &mut InstState, op: u16, slots: &mut [u64], force_async: bool) -> u64 {
    let Some(cmd) = COMMANDS.get(op as usize) else { return VK_ERROR_INITIALIZATION_FAILED as u32 as u64 };
    let params = cmd.params();
    let fail = |e: i32| -> u64 { e as u32 as u64 };
    let args = slots.as_ptr() as *const u8;

    // Księgowość deskryptorów: układy i pule zapamiętujemy, a alokację zestawów odpowiadamy od razu tylko wtedy, gdy pula na pewno starczy.
    let mut speculated = false;
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
            speculated = st.desc.try_allocate(pool, &layouts, pnext);
            alloc_ctx = Some((pool, layouts));
        }
        _ => {}
    }
    // Wymagania pamięci: identyczne obrazy i bufory dostają odpowiedź z cache, bez obiegu po sieci.
    if let Some(done) = memreq_cached(st, cmd.name, slots) {
        return done;
    }
    // Aktualizacja deskryptorów szablonem: dane zawierają uchwyty, więc rozwijamy ją do zwykłego vkUpdateDescriptorSets.
    if cmd.name == "vkUpdateDescriptorSetWithTemplate" {
        return update_with_template(st, slots);
    }
    // Kolejka wydana już wcześniej (to samo urządzenie, rodzina, indeks) wraca bez wysyłania.
    if cmd.name == "vkGetDeviceQueue" {
        let dev = IcdCtx.handle_to_wire("VkDevice", slots[0]).unwrap_or(0);
        if let Some(q) = st.queues.get(&(dev, slots[1] as u32, slots[2] as u32)) {
            std::ptr::write_unaligned(slots[3] as *mut usize, *q);
            return 0;
        }
    }
    // Uchwyty wyjściowe nadaje klient: wpisujemy identyfikatory w pamięć aplikacji przed zakodowaniem.
    let mut disp_outs: Vec<(*mut u64, u64)> = Vec::new();
    let Ok(outs) = out_handles(cmd, args) else { return fail(VK_ERROR_INITIALIZATION_FAILED) };
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
        st.write_mapped();
        st.gpu_dirty = true;
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
        return fail(VK_ERROR_INITIALIZATION_FAILED);
    }
    let bytes = e.finish();
    // `vkGetDescriptorEXT` (VK_EXT_descriptor_buffer): bajty deskryptora to funkcja zakodowanego żądania, więc powtórzenia idą z cache.
    let desc_key = (cmd.name == "vkGetDescriptorEXT" && slots.len() >= 4 && slots[3] != 0 && slots[2] <= DESC_CACHE_MAX_BYTES).then(|| bytes.clone());
    if let Some(key) = &desc_key {
        if let Some(out) = st.desc_cache.get(key) {
            if out.len() as u64 == slots[2] {
                std::ptr::copy_nonoverlapping(out.as_ptr(), slots[3] as *mut u8, out.len());
                st.stats.desc_hits += 1;
                return 0;
            }
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
            let cached = is_cached_type(st, slots, cmd, args);
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
        let obj = Box::into_raw(Box::new(DispObj { loader_data: 0, id }));
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
        st.gpuw.memory_freed(slots[1]);
    }

    st.stats.commands += 1;
    let has_data_out = params.iter().any(|p| p.shape == Shape::Pointer && !p.is_const && p.elem != Elem::Handle && p.ptr_depth == 1);
    if force_async || (cmd.class != CmdClass::Sync && !has_data_out && cmd.ret != Ret::U64 && !is_pipeline_probe(cmd.name, slots) && !(may_fail_legitimately(cmd.name) && !speculated)) {
        st.batch.extend(bytes);
        if st.batch.len() > BATCH_FLUSH {
            st.flush();
        }
        return 0;
    }
    // Droga synchroniczna: partia, potem polecenie, potem `Sync` (błąd asynchroniczny); jeden obieg po sieci.
    st.flush();
    st.stats.query_bytes += bytes.len() as u64;
    st.stats.roundtrips += 1;
    // Po oczekiwaniu trzeba odświeżyć odwzorowane pamięci: żądania wysyłamy zaraz za `Sync`, w tym samym obiegu po sieci.
    let pre_ids: Vec<u64> = if cmd.class == CmdClass::Sync && st.gpu_dirty { st.refresh_ids() } else { Vec::new() };
    let mut ok = st.client.send(&Request::Query { data: bytes }).is_ok() && st.client.send(&Request::Sync).is_ok();
    for id in &pre_ids {
        let req = st.delta_request(*id);
        ok = ok && st.client.send(&req).is_ok();
    }
    let t_recv = std::time::Instant::now();
    let (q, sy) = if ok { (st.client.recv(), st.client.recv()) } else { (Err(gpu_renderd::client::ClientError::Closed), Err(gpu_renderd::client::ClientError::Closed)) };
    // Odpowiedzi na odświeżenia przychodzą zawsze, więc czytamy je od razu (także przy późniejszym błędzie), żeby nie rozsynchronizować strumienia.
    let t_refresh = std::time::Instant::now();
    let mut refreshed = false;
    if ok {
        for id in &pre_ids {
            if let Ok(Response::Data(d)) = st.client.recv() {
                if st.apply_delta(*id, &d) {
                    st.drain_delta(*id);
                }
                refreshed = true;
            }
        }
    }
    if refreshed {
        st.stats.refreshes += 1;
        st.stats.t_refresh_ns += t_refresh.elapsed().as_nanos() as u64;
    }
    st.stats.t_syncrecv_ns += t_recv.elapsed().as_nanos() as u64;
    if let Ok(Response::Synced { error }) = sy {
        if error < 0 {
            st.lose();
        }
    }
    let Ok(Response::Data(d)) = q else {
        st.lose();
        return fail(VK_ERROR_DEVICE_LOST);
    };
    let result = d.get(..4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(VK_ERROR_DEVICE_LOST);
    let mut dec = Decoder::new(d.get(4..).unwrap_or(&[]));
    if result >= 0 && decode_out_params(cmd, args, &mut dec).is_err() {
        return fail(VK_ERROR_INITIALIZATION_FAILED);
    }
    if cmd.ret == Ret::U64 {
        // Wartość zwracana (np. adres bufora) idzie na końcu odpowiedzi; brak = 0, czyli adres nieprawidłowy.
        return if result >= 0 { dec.get_u64().unwrap_or(0) } else { 0 };
    }
    if result >= 0 {
        memreq_store(st, cmd.name, slots);
        if let Some(key) = desc_key {
            if st.desc_cache.len() >= DESC_CACHE_MAX_ENTRIES {
                st.desc_cache.clear();
            }
            st.desc_cache.insert(key, std::slice::from_raw_parts(slots[3] as *const u8, slots[2] as usize).to_vec());
        }
    }
    if let Some((pool, layouts)) = &alloc_ctx {
        if !speculated {
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
            std::ptr::write_unaligned(slots[3] as *mut u64, 0);
        }
    }
    if cmd.class == CmdClass::Sync && result == 0 {
        if refreshed {
            // Pamięci zostały odświeżone w tym samym obiegu co oczekiwanie.
            st.gpu_dirty = false;
        } else {
            st.read_mapped();
        }
    }
    fail(result)
}

// ---------- pamięć odwzorowana (lokalnie; synchronizacja z serwerem w punktach) ----------

pub unsafe extern "C" fn map_memory(_dev: u64, mem: u64, offset: u64, size: u64, _flags: u32, out: *mut *mut u8) -> i32 {
    let Some(inst) = state() else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut st = lock(&inst.state);
    let Some(total) = st.mem_sizes.get(&mem).copied() else { return VK_ERROR_MEMORY_MAP_FAILED };
    if offset > total || (size != u64::MAX && offset.saturating_add(size) > total) || out.is_null() {
        return VK_ERROR_MEMORY_MAP_FAILED;
    }
    let sh = st.mems.entry(mem).or_insert_with(|| Shadow::new(total as usize));
    sh.mapped = true;
    *out = sh.data.as_mut_ptr().add(offset as usize);
    0
}

pub unsafe extern "C" fn unmap_memory(_dev: u64, mem: u64) {
    if let Some(inst) = state() {
        let mut st = lock(&inst.state);
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
    let mut st = lock(&inst.state);
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
    let mut st = lock(&inst.state);
    let mut map: HashMap<u64, ()> = HashMap::new();
    for (mem, off, size) in ranges(count, r) {
        let Some(len) = st.mems.get(&mem).map(|s| s.data.len() as u64) else { continue };
        let end = if size == u64::MAX { len } else { off.saturating_add(size).min(len) };
        if let Some(d) = st.read_mem(mem, off, (end - off) as usize) {
            if let Some(s) = st.mems.get_mut(&mem) {
                s.data[off as usize..end as usize].copy_from_slice(&d);
                s.mark_synced(off as usize, end as usize);
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

}
