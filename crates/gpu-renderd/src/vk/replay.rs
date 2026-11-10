//! Odtwarzanie poleceń Vulkana z protokołu na prawdziwym sterowniku serwera.
//!
//! Każde polecenie wywołujemy uniwersalnym thunkiem: wszystkie parametry całkowite i wskaźniki idą jako `u64`
//! (6 pierwszych w rejestrach, reszta na stosie, zgodnie z System V x86_64), a `float`/`double` w rejestrach `xmm`.
//! Dzięki temu jedna ścieżka obsługuje każde polecenie opisane w metadanych, bez ręcznych funkcji na polecenie.
//!
//! Zaufanie: uwierzytelniony klient może wysłać polecenia niepoprawne wg Vulkana (np. kopie poza zakresem), a bez
//! warstw walidacyjnych sterownik może wtedy zawiesić proces. Granicą zaufania jest token (patrz AGENTS.md).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::Arc;

use gpu_proto::meta::{find_command, CommandMeta, Elem, Ret, Shape};
use gpu_vkcodec::codec::{arg_uint, decode_command, encode_out_params, out_handles, Ctx, DecodedCommand, Identity};
use gpu_vkcodec::CodecError;
use gpu_wire::{Decoder, Encoder};

use crate::handler::DeltaItem;
use crate::handler::ERR_UNKNOWN;
use crate::vk::loader::{LibVulkan, VkInstance};
use crate::vk::registry::{Entry, Registry};

mod copy;

const MAX_INTS: usize = 14;
const MAX_FLOATS: usize = 8;
const VK_WHOLE_SIZE: u64 = u64::MAX;
const ST_MAPPED_MEMORY_RANGE: u32 = 6;

type Thunk = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, f64, f64, f64, f64, f64, f64, f64, f64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;

/// Wywołuje funkcję C z parametrami całkowitymi `ints` i zmiennoprzecinkowymi `floats` (kolejność jak w deklaracji).
///
/// # Safety
/// `f` musi być wskaźnikiem na funkcję o zgodnej liczbie i rodzajach parametrów, a wskaźniki w `ints` poprawne.
pub unsafe fn call_thunk(f: *const c_void, ints: &[u64], floats: &[f64]) -> Option<u64> {
    if ints.len() > MAX_INTS || floats.len() > MAX_FLOATS {
        return None;
    }
    let mut i = [0u64; MAX_INTS];
    i[..ints.len()].copy_from_slice(ints);
    let mut x = [0f64; MAX_FLOATS];
    x[..floats.len()].copy_from_slice(floats);
    let t: Thunk = std::mem::transmute::<*const c_void, Thunk>(f);
    Some(t(i[0], i[1], i[2], i[3], i[4], i[5], x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7], i[6], i[7], i[8], i[9], i[10], i[11], i[12], i[13]))
}

/// Tłumacz uchwytów, który zapamiętuje identyfikatory z sieci (do wyrejestrowania po `vkDestroy*`/`vkFree*`).
struct Recording<'a> {
    reg: &'a Registry,
    seen: RefCell<Vec<u64>>,
}

impl Ctx for Recording<'_> {
    fn handle_to_wire(&self, ty: &str, raw: u64) -> Result<u64, CodecError> {
        self.reg.handle_to_wire(ty, raw)
    }
    fn wire_to_handle(&self, ty: &str, wire: u64) -> Result<u64, CodecError> {
        self.seen.borrow_mut().push(wire);
        let r = self.reg.wire_to_handle(ty, wire);
        if r.is_err() {
            // Typ nieznanego uchwytu pozwala odróżnić zestaw deskryptorów od potoku, obrazu itd.
            eprintln!("replay: nieznany uchwyt {wire} typu {ty}");
        }
        r
    }
}

/// Nazwa polecenia z pierwszych dwóch bajtów (kod operacji) fragmentu strumienia: do komunikatów o błędach dekodowania.
fn peek_name(data: &[u8]) -> &'static str {
    data.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]])).and_then(|op| gpu_proto::meta::COMMANDS.get(op as usize)).map_or("?", |c| c.name)
}

/// Czas wykonania poleceń na serwerze według nazwy (liczba, ns) i czas samego dekodowania: do rankingu w logu statystyk.
#[derive(Default)]
struct CmdTimes {
    by_name: HashMap<&'static str, (u64, u64)>,
    decode_ns: u64,
}

impl CmdTimes {
    fn merge(&mut self, o: CmdTimes) {
        self.decode_ns += o.decode_ns;
        for (k, (n, ns)) in o.by_name {
            let e = self.by_name.entry(k).or_insert((0, 0));
            e.0 += n;
            e.1 += ns;
        }
    }
}

static CMD_TIMES: std::sync::LazyLock<std::sync::Mutex<CmdTimes>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(CmdTimes::default()));

/// Ranking poleceń według łącznego czasu wykonania w `Replayer::execute`: `nazwa liczba ms (średnio µs)`, oraz czas dekodowania.
pub fn cmd_times_report() -> String {
    let t = CMD_TIMES.lock().unwrap_or_else(|e| e.into_inner());
    let mut v: Vec<_> = t.by_name.iter().collect();
    v.sort_by_key(|(_, (_, ns))| std::cmp::Reverse(*ns));
    let top: Vec<String> = v.iter().take(10).map(|(n, (c, ns))| format!("{} {}x {}ms ({:.1}µs)", n.trim_start_matches("vk"), c, ns / 1_000_000, *ns as f64 / 1e3 / (*c).max(1) as f64)).collect();
    format!("dekodowanie {} ms; {}", t.decode_ns / 1_000_000, top.join("; "))
}

pub struct Replayer {
    lib: Arc<LibVulkan>,
    pub registry: Registry,
    instance: VkInstance,
    fns: HashMap<u16, *const c_void>,
    maps: HashMap<u64, *mut u8>,
    /// Lustro "ostatnio przesłane w to miejsce" po pamięci (RAM serwera), słownik dla zapisów `MemWriteZstdPfx`: zeros na początku, każdy zapis pamięci
    /// kopiuje tu swoje bajty, więc jest identyczne z lustrem klienta niezależnie od tego, co GPU zrobiło z pamięcią.
    mirror: HashMap<u64, Vec<u8>>,
    /// Stan stron pamięci znany klientowi (do odczytu samych różnic).
    pages: HashMap<u64, gpu_proto::pagediff::PageTracker>,
    /// Surowa wartość zwrócona przez ostatnio wykonane polecenie (dla poleceń `Ret::U64`).
    last_ret: u64,
    /// Nazwa ostatnio wykonanego polecenia (do komunikatów o błędzie dekodowania następnego).
    last_name: &'static str,
    /// Opis `VkDescriptorPoolCreateInfo` po prawdziwym uchwycie puli i licznik nieudanych alokacji (diagnostyka odmów `OUT_OF_POOL_MEMORY`).
    pool_desc: HashMap<u64, String>,
    /// Pula deskryptorów (identyfikator klienta) -> zestawy z niej zarejestrowane. `vkResetDescriptorPool` i `vkDestroyDescriptorPool` zwalniają
    /// wszystkie zestawy naraz, więc muszą zdjąć je też z rejestru (inaczej po ok. milionie alokacji rejestr się zapełnia i gra pada).
    pool_sets: HashMap<u64, Vec<u64>>,
    failed_allocs: u32,
    /// Bufor na kopię odczytywanej pamięci (patrz `stage`), używany wielokrotnie.
    scratch: Vec<u8>,
    /// Odczyt pamięci przez kopię GPU (patrz `copy`).
    copy: copy::GpuCopy,
}

// SAFETY: wskaźniki to uchwyty i adresy odwzorowań sterownika używane wyłącznie przez wątek sesji.
unsafe impl Send for Replayer {}

/// `GPU_RENDERD_TRACE=<plik>`: dopisuje nazwę każdego polecenia tuż PRZED wykonaniem w sterowniku, więc po awarii sterownika
/// ostatnia linia wskazuje sprawcę (diagnostyka; wolne, tylko do debugowania).
fn trace_command(name: &str) {
    static PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    if let Some(path) = PATH.get_or_init(|| std::env::var("GPU_RENDERD_TRACE").ok()) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{name}");
        }
    }
}

fn trace_text(line: &str) {
    if let Ok(path) = std::env::var("GPU_RENDERD_TRACE") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Do tylu bajtów (łącznie) odczytywane zakresy kopiujemy strumieniowo do zwykłej pamięci i dopiero z niej liczymy skróty:
/// odczyty pamięci write-combined albo przez PCIe są tam o rząd wielkości wolniejsze (patrz `stream`). Większe odczyty idą bez kopii
/// (to pamięci HOST_CACHED, które klient odświeża także, gdy są duże).
const STAGE_MAX_BYTES: usize = 32 << 20;

/// Skąd czytać dane do porównania: z pamięci odwzorowanej albo z kopii w `scratch`.
enum Staged {
    Mapped(*const u8, usize),
    Scratch(usize),
}

impl Staged {
    fn get<'a>(&self, scratch: &'a [u8]) -> &'a [u8] {
        match *self {
            // SAFETY: wskaźnik i długość pochodzą z odwzorowania, które żyje do końca wywołania (patrz `stage`).
            Staged::Mapped(p, n) => unsafe { std::slice::from_raw_parts(p, n) },
            Staged::Scratch(n) => &scratch[..n],
        }
    }
}

/// Przygotowuje dane do porównania obszaru `ptr[..size]`: czytane są tylko `spans` (przedziały bajtów).
///
/// # Safety
/// `ptr` musi wskazywać na odwzorowanie o długości `size`, ważne do końca użycia wyniku.
unsafe fn stage(ptr: *const u8, size: usize, spans: &[(usize, usize)], mut scratch: Vec<u8>) -> (Staged, Vec<u8>) {
    let total: usize = spans.iter().map(|(a, b)| b - a).sum();
    if total == 0 || total > STAGE_MAX_BYTES {
        return (Staged::Mapped(ptr, size), scratch);
    }
    if scratch.len() < size {
        scratch.resize(size, 0);
    }
    for &(a, b) in spans {
        crate::vk::stream::stream_copy(scratch.as_mut_ptr().add(a), ptr.add(a), b - a);
    }
    (Staged::Scratch(size), scratch)
}

fn code(e: CodecError) -> i32 {
    eprintln!("replay: {e:?}");
    ERR_UNKNOWN
}

impl Replayer {
    pub fn new(lib: Arc<LibVulkan>) -> Replayer {
        Replayer { lib, registry: Registry::new(), instance: std::ptr::null_mut(), fns: HashMap::new(), maps: HashMap::new(), mirror: HashMap::new(), pages: HashMap::new(), last_ret: 0, last_name: "", pool_desc: HashMap::new(), pool_sets: HashMap::new(), failed_allocs: 0, scratch: Vec::new(), copy: Default::default() }
    }

    pub fn registered_handles(&self) -> usize {
        self.registry.len()
    }

    pub fn set_instance(&mut self, raw: VkInstance) {
        self.instance = raw;
        self.fns.clear();
    }

    fn func(&mut self, op: u16, name: &str) -> Result<*const c_void, i32> {
        if let Some(f) = self.fns.get(&op) {
            return Ok(*f);
        }
        if self.instance.is_null() {
            return Err(ERR_UNKNOWN);
        }
        let c = CString::new(name).map_err(|_| ERR_UNKNOWN)?;
        // SAFETY: instancja jest ważna (ustawiona przez backend po udanym `vkCreateInstance`).
        let f = unsafe { self.lib.proc_addr(self.instance, &c) }.ok_or(ERR_UNKNOWN)?;
        self.fns.insert(op, f);
        Ok(f)
    }

    fn func_by_name(&mut self, name: &str) -> Result<*const c_void, i32> {
        let (op, _) = find_command(name).ok_or(ERR_UNKNOWN)?;
        self.func(op, name)
    }

    /// Wykonuje partię poleceń; przerywa na pierwszym błędzie.
    pub fn execute(&mut self, batch: &[u8]) -> Result<(), i32> {
        let mut dec = Decoder::new(batch);
        let mut local = CmdTimes::default();
        let result = loop {
            let t0 = std::time::Instant::now();
            let rec = Recording { reg: &self.registry, seen: RefCell::new(Vec::new()) };
            let at = batch.len() - dec.remaining();
            let dc = match decode_command(&mut dec, &rec) {
                Ok(Some(dc)) => dc,
                Ok(None) => break Ok(()),
                Err(e) => {
                    eprintln!("replay: błąd dekodowania polecenia {} (następnego po {}): {e:?}", peek_name(&batch[at..]), self.last_name);
                    break Err(code(e));
                }
            };
            let seen = rec.seen.into_inner();
            let t1 = std::time::Instant::now();
            local.decode_ns += (t1 - t0).as_nanos() as u64;
            let r = self.run(&dc, &seen, false);
            let e = local.by_name.entry(dc.command.name).or_insert((0, 0));
            e.0 += 1;
            e.1 += t1.elapsed().as_nanos() as u64;
            if let Err(c) = r {
                break Err(c);
            }
        };
        CMD_TIMES.lock().unwrap_or_else(|e| e.into_inner()).merge(local);
        result
    }

    /// Wykonuje jedno polecenie. Odpowiedź: kod wyniku (`i32` LE, także ujemny i dodatni jak `VK_NOT_READY`),
    /// potem zakodowane parametry wyjściowe. Ujemny kod nie rejestruje uchwytów i nie koduje wyników.
    pub fn query(&mut self, data: &[u8]) -> Result<Vec<u8>, i32> {
        let mut dec = Decoder::new(data);
        let rec = Recording { reg: &self.registry, seen: RefCell::new(Vec::new()) };
        let dc = match decode_command(&mut dec, &rec) {
            Ok(Some(dc)) => dc,
            Ok(None) => return Err(ERR_UNKNOWN),
            Err(e) => {
                eprintln!("replay: błąd dekodowania zapytania {} (poprzednie polecenie {}): {e:?}", peek_name(data), self.last_name);
                return Err(code(e));
            }
        };
        let seen = rec.seen.into_inner();
        if dec.remaining() != 0 {
            return Err(ERR_UNKNOWN);
        }
        let result = self.run(&dc, &seen, true)?;
        let mut enc = Encoder::new();
        enc.put_u32(result as u32);
        if result >= 0 {
            // SAFETY: `dc.args` to świeżo odtworzone i wykonane parametry tego polecenia.
            unsafe { encode_out_params(dc.command, dc.args.ptr, &mut enc, &Identity) }.map_err(code)?;
            if dc.command.ret == Ret::U64 {
                enc.put_u64(self.last_ret);
            }
        }
        Ok(enc.finish())
    }

    /// Zwraca kod wyniku polecenia (0 dla `void`). `tolerate`: ujemny kod jest wynikiem, a nie błędem partii.
    fn run(&mut self, dc: &DecodedCommand, seen: &[u64], tolerate: bool) -> Result<i32, i32> {
        let cmd = dc.command;
        let args = dc.args.ptr as *const u8;
        let f = self.func(dc.opcode, cmd.name)?;
        let (mut ints, mut floats) = (Vec::new(), Vec::new());
        for p in cmd.params() {
            // SAFETY: sloty parametrów mają 8 B i leżą w arenie o rozmiarze 8 * liczba parametrów.
            let slot = unsafe { std::ptr::read_unaligned(args.add(p.offset as usize) as *const u64) };
            match (p.shape, p.elem, p.type_name) {
                (Shape::Value, Elem::Scalar, "float") => floats.push(f64::from_bits(slot & 0xFFFF_FFFF)),
                (Shape::Value, Elem::Scalar, "double") => floats.push(f64::from_bits(slot)),
                _ => ints.push(slot),
            }
        }
        let parent = cmd
            .params()
            .first()
            .filter(|p| p.shape == Shape::Value && p.elem == Elem::Handle)
            .map(|p| ints.first().copied().filter(|_| p.offset == 0).unwrap_or(0))
            .unwrap_or(0);
        // SAFETY: `args` wskazuje na sloty tego polecenia; wskaźniki w nich pochodzą z areny dekodera.
        let outs = unsafe { out_handles(cmd, args) }.map_err(code)?;
        let ids: Vec<Vec<u64>> = outs.iter().map(|(_, p, n)| (0..*n).map(|i| unsafe { std::ptr::read_unaligned(p.add(i)) }).collect()).collect();
        let size = if cmd.name == "vkAllocateMemory" { unsafe { arg_uint(cmd, args, "pAllocateInfo->allocationSize") }.unwrap_or(0) } else { 0 };
        self.last_name = cmd.name;
        trace_command(cmd.name);
        // Rozmiary rysowań i dispatchy w śladzie: zawieszenie GPU (pętla albo gigantyczny dispatch z nieaktualnych danych) widać po absurdalnych liczbach.
        if matches!(cmd.name, "vkCmdDispatch" | "vkCmdDraw" | "vkCmdDrawIndexed" | "vkCmdDispatchIndirect" | "vkCmdDrawIndirect" | "vkCmdDrawIndexedIndirect" | "vkCmdDispatchBase") && std::env::var_os("GPU_RENDERD_TRACE").is_some() {
            trace_text(&format!("  args {} {:?}", cmd.name, &ints[1.min(ints.len())..ints.len().min(8)]));
        }
        if std::env::var("GPU_RENDERD_TRACE").is_ok() && (cmd.name == "vkCreateDescriptorPool" || cmd.name == "vkAllocateDescriptorSets") {
            // SAFETY: parametr 1 to wskaźnik na zdekodowaną strukturę w arenie.
            unsafe {
                let ptr = std::ptr::read_unaligned(args.add(8) as *const u64) as *const u8;
                let d = if cmd.name == "vkCreateDescriptorPool" { gpu_proto::debug::describe_descriptor_pool_info(ptr) } else { gpu_proto::debug::describe_descriptor_set_alloc(ptr) };
                trace_text(&format!("  {} {d}", cmd.name));
            }
        }
        if cmd.name == "vkCreateGraphicsPipelines" && std::env::var("GPU_RENDERD_TRACE").is_ok() {
            // SAFETY: sloty parametrów (8 B każdy) leżą w arenie; parametr 3 to pCreateInfos, 2 to createInfoCount.
            unsafe {
                let slot = |i: usize| std::ptr::read_unaligned(args.add(i * 8) as *const u64);
                for i in 0..slot(2).min(8) as usize {
                    trace_text(&format!("  pipeline[{i}] server: {}", gpu_proto::debug::describe_graphics_pipeline_info((slot(3) as *const u8).add(i * 144))));
                }
            }
        }
        // Obiekty pomocnicze kopii GPU zwalniamy przed obiektami, na których wiszą (inaczej zostaną w sterowniku).
        if cmd.name == "vkDestroyDevice" {
            self.copy_forget_device(ints[0]);
        } else if cmd.name == "vkFreeMemory" && ints.len() >= 2 {
            self.copy_forget_memory(ints[0], ints[1]);
        }
        // SAFETY: `f` to funkcja Vulkana o nazwie polecenia, a parametry odpowiadają jego deklaracji (metadane z vk.xml).
        let t0 = std::time::Instant::now();
        let rax = unsafe { call_thunk(f, &ints, &floats) }.ok_or(ERR_UNKNOWN)?;
        trace_text(&format!("  t {} {}us", cmd.name, t0.elapsed().as_micros()));
        if cmd.name == "vkCreateDescriptorPool" && std::env::var("GPU_RENDERD_TRACE").is_ok() {
            // SAFETY: parametr 3 to wskaźnik wyjściowy na uchwyt puli (sterownik właśnie go wpisał).
            let pool = unsafe { std::ptr::read_unaligned(std::ptr::read_unaligned(args.add(24) as *const u64) as *const u64) };
            trace_text(&format!("  pool utworzona: {pool:#x}"));
        }
        self.last_ret = rax;
        let result = if cmd.ret == Ret::VkResult { rax as u32 as i32 } else { 0 };
        if result >= 0 && cmd.name == "vkCreateDescriptorPool" {
            // SAFETY: parametr 1 to zdekodowana struktura w arenie, parametr 3 wskaźnik na uchwyt, który sterownik właśnie wpisał.
            unsafe {
                let info = std::ptr::read_unaligned(args.add(8) as *const u64) as *const u8;
                let pool = std::ptr::read_unaligned(std::ptr::read_unaligned(args.add(24) as *const u64) as *const u64);
                self.pool_desc.insert(pool, gpu_proto::debug::describe_descriptor_pool_info(info));
            }
        }
        if result < 0 && cmd.name == "vkAllocateDescriptorSets" && self.failed_allocs < 20 {
            self.failed_allocs += 1;
            // SAFETY: parametr 1 to zdekodowane `VkDescriptorSetAllocateInfo` (pula @16 jako prawdziwy uchwyt).
            unsafe {
                let info = std::ptr::read_unaligned(args.add(8) as *const u64) as *const u8;
                let pool = std::ptr::read_unaligned(info.add(16) as *const u64);
                eprintln!("replay: odmowa alokacji {result}: {} ; pula: {}", gpu_proto::debug::describe_descriptor_set_alloc(info), self.pool_desc.get(&pool).map_or("(nieznana)", |s| s.as_str()));
            }
        }
        if result < 0 {
            eprintln!("replay: {} zwróciło {result}", cmd.name);
            return if tolerate { Ok(result) } else { Err(result) };
        }
        if cmd.name == "vkGetDeviceQueue" && ints.len() >= 4 {
            // SAFETY: parametr 3 to wskaźnik wyjściowy na uchwyt kolejki (sterownik właśnie go wpisał).
            let q = unsafe { std::ptr::read_unaligned(ints[3] as *const u64) };
            self.copy.queues.entry(ints[0]).or_insert((q, ints[1] as u32));
        }
        if cmd.name == "vkAllocateMemory" {
            // SAFETY: `args` to sloty tego polecenia; `pAllocateInfo->memoryTypeIndex` czytamy z zdekodowanej struktury.
            if let (Some(t), Some(id)) = (unsafe { arg_uint(cmd, args, "pAllocateInfo->memoryTypeIndex") }, ids.first().and_then(|v| v.first())) {
                self.copy.mem_types.insert(*id, t as u32);
            }
        }
        let set_ids: Vec<u64> = if cmd.name == "vkAllocateDescriptorSets" { ids.first().cloned().unwrap_or_default() } else { Vec::new() };
        for ((kind, p, _), ids) in outs.iter().zip(ids) {
            for (i, id) in ids.iter().enumerate() {
                // SAFETY: sterownik wpisał prawdziwy uchwyt w miejsce identyfikatora klienta.
                let real = unsafe { std::ptr::read_unaligned(p.add(i)) };
                // Sterownik zostawia VK_NULL_HANDLE, gdy np. potok wymaga kompilacji (VK_PIPELINE_COMPILE_REQUIRED): takiego
                // uchwytu nie rejestrujemy, więc późniejsze użycie jest błędem, a nie wywołaniem sterownika z null.
                if real == 0 {
                    continue;
                }
                self.registry.register(*id, Entry { kind, real, parent, size }).map_err(code)?;
            }
        }
        if cmd.name == "vkAllocateDescriptorSets" && seen.len() >= 2 {
            // seen: [urządzenie, pula, układy...]; zestawy z tej alokacji należą do puli seen[1].
            self.pool_sets.entry(seen[1]).or_default().extend(set_ids);
        }
        self.unregister_after(cmd, seen);
        Ok(result)
    }

    /// `vkDestroy*` i `vkFree*` zdejmują obiekt z rejestru (identyfikatory z sieci, nie prawdziwe uchwyty).
    fn unregister_after(&mut self, cmd: &CommandMeta, seen: &[u64]) {
        let n = cmd.name;
        if (n == "vkResetDescriptorPool" || n == "vkDestroyDescriptorPool") && seen.len() >= 2 {
            for id in self.pool_sets.remove(&seen[1]).unwrap_or_default() {
                self.registry.unregister(id);
            }
        }
        let ids: &[u64] = if n == "vkFreeCommandBuffers" || n == "vkFreeDescriptorSets" {
            seen.get(2..).unwrap_or(&[])
        } else if n == "vkDestroyDevice" || n == "vkDestroyInstance" {
            // Niszczony obiekt jest parametrem 0 (nie ma obiektu nadrzędnego).
            seen.get(..1).unwrap_or(&[])
        } else if n.starts_with("vkDestroy") || n == "vkFreeMemory" {
            seen.get(1..2).unwrap_or(&[])
        } else {
            &[]
        };
        for id in ids {
            self.registry.unregister(*id);
            self.maps.remove(id);
            self.mirror.remove(id);
            self.pages.remove(id);
            self.copy.mem_types.remove(id);
        }
    }

    fn mapping(&mut self, memory: u64) -> Result<(*mut u8, Entry, u64), i32> {
        let e = *self.registry.get(memory).filter(|e| e.kind == "VkDeviceMemory").ok_or(ERR_UNKNOWN)?;
        if let Some(p) = self.maps.get(&memory) {
            return Ok((*p, e, e.parent));
        }
        let f = self.func_by_name("vkMapMemory")?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        // SAFETY: vkMapMemory(device, memory, offset, size, flags, ppData): wszystkie wskaźniki żywe, uchwyty z rejestru.
        let r = unsafe { call_thunk(f, &[e.parent, e.real, 0, VK_WHOLE_SIZE, 0, &mut ptr as *mut *mut u8 as u64], &[]) }.ok_or(ERR_UNKNOWN)?;
        if (r as u32 as i32) < 0 || ptr.is_null() {
            return Err(r as u32 as i32);
        }
        self.maps.insert(memory, ptr);
        Ok((ptr, e, e.parent))
    }

    fn range_call(&mut self, name: &str, device: u64, memory_real: u64) -> Result<(), i32> {
        let f = self.func_by_name(name)?;
        let mut r = [0u64; 5];
        r[0] = ST_MAPPED_MEMORY_RANGE as u64;
        r[2] = memory_real;
        r[4] = VK_WHOLE_SIZE;
        // SAFETY: VkMappedMemoryRange (40 B) zbudowany na stosie, ważny przez czas wywołania.
        let res = unsafe { call_thunk(f, &[device, 1, r.as_ptr() as u64], &[]) }.ok_or(ERR_UNKNOWN)?;
        if (res as u32 as i32) < 0 {
            return Err(res as u32 as i32);
        }
        Ok(())
    }

    /// Zapisuje bajty do pamięci urządzenia (przez odwzorowanie); pamięć musi być widoczna dla hosta.
    pub fn mem_write(&mut self, memory: u64, offset: u64, data: &[u8]) -> Result<(), i32> {
        let (ptr, e, device) = self.mapping(memory)?;
        let end = offset.checked_add(data.len() as u64).ok_or(ERR_UNKNOWN)?;
        if end > e.size {
            return Err(ERR_UNKNOWN);
        }
        // SAFETY: zakres [offset, offset+len) mieści się w odwzorowanej pamięci o rozmiarze `e.size`.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(offset as usize), data.len()) };
        self.mirror.entry(memory).or_insert_with(|| vec![0u8; e.size as usize])[offset as usize..end as usize].copy_from_slice(data);
        // Klient ma te strony w takim stanie, jaki właśnie wysłał: odczyt różnic nie odeśle ich z powrotem.
        self.pages.entry(memory).or_insert_with(|| gpu_proto::pagediff::PageTracker::new(e.size as usize)).mark_data(offset as usize, data);
        self.range_call("vkFlushMappedMemoryRanges", device, e.real)
    }

    /// Zapis skompresowany z prefiksem: poprzednia treść tego zakresu z lustra jest słownikiem zstd. Wynik musi mieć dokładnie `raw_len` bajtów i zgodne
    /// crc32c (rozbieżność lustra klienta i serwera to błąd, nie cicha korupcja).
    pub fn mem_write_pfx(&mut self, memory: u64, offset: u64, raw_len: u32, raw_crc: u32, data: &[u8]) -> Result<(), i32> {
        let e = *self.registry.get(memory).filter(|e| e.kind == "VkDeviceMemory").ok_or(ERR_UNKNOWN)?;
        let end = offset.checked_add(raw_len as u64).filter(|x| *x <= e.size).ok_or(ERR_UNKNOWN)?;
        let raw = {
            let mirror = self.mirror.entry(memory).or_insert_with(|| vec![0u8; e.size as usize]);
            let prefix = &mirror[offset as usize..end as usize];
            let mut raw: Vec<u8> = Vec::with_capacity(raw_len as usize);
            let mut dctx = zstd::zstd_safe::DCtx::create();
            // Pojemność wyjścia ogranicza rozpakowanie (bomba dekompresyjna niemożliwa).
            if dctx.ref_prefix(prefix).is_err() || dctx.decompress(&mut raw, data).is_err() || raw.len() != raw_len as usize || gpu_proto::frame::crc32c(&raw) != raw_crc {
                return Err(ERR_UNKNOWN);
            }
            raw
        };
        self.mem_write(memory, offset, &raw)
    }

    /// Zapis kilku fragmentów jednej pamięci jednym odwzorowaniem i jednym `vkFlushMappedMemoryRanges` (patrz `Request::MemWriteRuns`).
    pub fn mem_write_runs(&mut self, memory: u64, runs: &[(u64, u32)], pages: &[(u32, u64)], raw: &[u8]) -> Result<(), i32> {
        let (ptr, e, device) = self.mapping(memory)?;
        let mut at = 0usize;
        for &(offset, len) in runs {
            let end = offset.checked_add(len as u64).filter(|x| *x <= e.size).ok_or(ERR_UNKNOWN)?;
            let next = at.checked_add(len as usize).filter(|x| *x <= raw.len()).ok_or(ERR_UNKNOWN)?;
            let part = &raw[at..next];
            // SAFETY: zakres [offset, end) mieści się w odwzorowanej pamięci o rozmiarze `e.size`.
            unsafe { std::ptr::copy_nonoverlapping(part.as_ptr(), ptr.add(offset as usize), part.len()) };
            self.pages.entry(memory).or_insert_with(|| gpu_proto::pagediff::PageTracker::new(e.size as usize)).mark_data(offset as usize, part);
            let _ = end;
            at = next;
        }
        // Strony dotknięte zapisem nie zostały przesłane w całości: ich skróty podaje klient (po zapisie), żeby odczyt różnic ich nie odsyłał.
        if let Some(t) = self.pages.get_mut(&memory) {
            for &(i, h) in pages {
                t.set_hash(i as usize, h);
            }
        }
        self.range_call("vkFlushMappedMemoryRanges", device, e.real)
    }

    /// Odczyt różnic: tylko strony, które zmieniły się od ostatniego zapisu lub odczytu tej pamięci (limit jednej odpowiedzi
    /// to `max_bytes`; flaga `more` mówi, że trzeba zapytać ponownie). Format: `pagediff::encode_segments`.
    pub fn mem_read_delta(&mut self, memory: u64, max_bytes: usize) -> Result<Vec<u8>, i32> {
        let (ptr, e, device) = self.mapping(memory)?;
        self.range_call("vkInvalidateMappedMemoryRanges", device, e.real)?;
        let size = e.size as usize;
        let spans = [(0usize, size)];
        // Cała pamięć bez cache (okno BAR) czytana procesorem to ok. 380 MB/s: kopia przez GPU (patrz `copy`).
        let gpu = if self.mem_is_host_cached(memory) { None } else { self.gpu_staged(memory, &spans) };
        // SAFETY: odwzorowanie ma `e.size` bajtów.
        let (data, scratch) = match gpu {
            Some(d) => (d, std::mem::take(&mut self.scratch)),
            None => unsafe { stage(ptr, size, &spans, std::mem::take(&mut self.scratch)) },
        };
        let tracker = self.pages.entry(memory).or_insert_with(|| gpu_proto::pagediff::PageTracker::new(size));
        let (ranges, more) = tracker.changed(data.get(&scratch), max_bytes);
        let out = gpu_proto::pagediff::encode_segments(data.get(&scratch), &ranges, more);
        self.scratch = scratch;
        Ok(out)
    }

    /// Jak `mem_read_delta`, ale tylko dla stron w `ranges`: pamięć poza nimi nie jest czytana ani haszowana.
    pub fn mem_read_delta_ranges(&mut self, memory: u64, ranges: &[(u64, u64)], max_bytes: usize, cached: bool) -> Result<Vec<u8>, i32> {
        let (ptr, e, device) = self.mapping(memory)?;
        self.range_call("vkInvalidateMappedMemoryRanges", device, e.real)?;
        let size = e.size as usize;
        let spans = gpu_proto::pagediff::page_spans(ranges, size);
        // Pamięć HOST_CACHED czyta się szybko: kopia pośrednia (potrzebna tylko dla wolnej pamięci write-combined) byłaby stratą.
        let spans_for_stage: &[(usize, usize)] = if cached { &[] } else { &spans };
        let gpu = if cached { None } else { self.gpu_staged(memory, &spans) };
        // SAFETY: odwzorowanie ma `e.size` bajtów.
        let (data, scratch) = match gpu {
            Some(d) => (d, std::mem::take(&mut self.scratch)),
            None => unsafe { stage(ptr, size, spans_for_stage, std::mem::take(&mut self.scratch)) },
        };
        let tracker = self.pages.entry(memory).or_insert_with(|| gpu_proto::pagediff::PageTracker::new(size));
        let (changed, more) = tracker.changed_in(data.get(&scratch), ranges, max_bytes);
        let out = gpu_proto::pagediff::encode_segments(data.get(&scratch), &changed, more);
        self.scratch = scratch;
        Ok(out)
    }

    /// Patrz `GpuBackend::mem_prefetch`.
    pub fn mem_prefetch(&mut self, items: &[DeltaItem]) {
        self.gpu_prefetch(items);
    }

    pub fn mem_prefetch_end(&mut self) {
        self.gpu_prefetch_end();
    }

    pub fn mem_read(&mut self, memory: u64, offset: u64, len: u32) -> Result<Vec<u8>, i32> {
        let (ptr, e, device) = self.mapping(memory)?;
        let end = offset.checked_add(len as u64).ok_or(ERR_UNKNOWN)?;
        if end > e.size {
            return Err(ERR_UNKNOWN);
        }
        self.range_call("vkInvalidateMappedMemoryRanges", device, e.real)?;
        // SAFETY: jak w `mem_write`.
        Ok(unsafe { std::slice::from_raw_parts(ptr.add(offset as usize), len as usize) }.to_vec())
    }

    /// Niszczy wszystkie urządzenia sesji (po `vkDeviceWaitIdle`); wywoływane przed zniszczeniem instancji.
    pub fn destroy_devices(&mut self) {
        let devices: Vec<(u64, Entry)> = self.registry_devices();
        for (id, e) in devices {
            if let (Ok(wait), Ok(destroy)) = (self.func_by_name("vkDeviceWaitIdle"), self.func_by_name("vkDestroyDevice")) {
                // SAFETY: uchwyt urządzenia jest ważny do chwili zniszczenia; po nim wpis znika z rejestru.
                unsafe {
                    call_thunk(wait, &[e.real], &[]);
                    // Sterownik NVIDIA nie zwalnia pamięci urządzenia przy `vkDestroyDevice`, gdy sesja kończy się bez zwolnienia obiektów (zerwane
                    // połączenie, zabita gra): po jednym przebiegu GravityMarka zostawało 2 GB VRAM, po awarii Cyberpunka 15 GB. Zwalniamy więc
                    // wszystkie alokacje tego urządzenia sami (po `vkDeviceWaitIdle` nic z nich nie korzysta).
                    if let (Ok(unmap), Ok(free)) = (self.func_by_name("vkUnmapMemory"), self.func_by_name("vkFreeMemory")) {
                        for (mid, m) in self.registry.entries_of_kind("VkDeviceMemory") {
                            if m.parent != e.real {
                                continue;
                            }
                            if self.maps.remove(&mid).is_some() {
                                call_thunk(unmap, &[e.real, m.real], &[]);
                            }
                            call_thunk(free, &[e.real, m.real, 0], &[]);
                            self.mirror.remove(&mid);
                            self.registry.unregister(mid);
                        }
                    }
                    call_thunk(destroy, &[e.real, 0], &[]);
                }
            }
            self.registry.unregister(id);
        }
        self.maps.clear();
        self.mirror.clear();
    }

    fn registry_devices(&self) -> Vec<(u64, Entry)> {
        self.registry.entries_of_kind("VkDevice")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn sum8(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {
        a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + 8 * h
    }

    unsafe extern "C" fn mixed(a: u64, x: f32, b: u64, y: f64, c: u64) -> u64 {
        (a + b + c) * 1000 + (x * 10.0) as u64 + (y * 100.0) as u64
    }

    unsafe extern "C" fn ret32(_a: u64) -> i32 {
        -7
    }

    #[test]
    fn thunk_passes_more_than_six_integer_arguments_in_order() {
        let r = unsafe { call_thunk(sum8 as *const () as *const c_void, &[1, 2, 3, 4, 5, 6, 7, 8], &[]) }.unwrap();
        assert_eq!(r, 1 + 4 + 9 + 16 + 25 + 36 + 49 + 64);
    }

    #[test]
    fn thunk_routes_floats_to_xmm_registers_independently_of_integers() {
        let x = f64::from_bits(2.5f32.to_bits() as u64); // float w dolnych 32 bitach xmm
        let r = unsafe { call_thunk(mixed as *const () as *const c_void, &[1, 2, 3], &[x, 1.5]) }.unwrap();
        assert_eq!(r, 6000 + 25 + 150);
    }

    #[test]
    fn thunk_returns_the_low_32_bits_for_vkresult_style_functions() {
        let r = unsafe { call_thunk(ret32 as *const () as *const c_void, &[0], &[]) }.unwrap();
        assert_eq!(r as u32 as i32, -7);
    }

    #[test]
    fn too_many_arguments_are_refused_not_truncated() {
        assert!(unsafe { call_thunk(sum8 as *const () as *const c_void, &[0; 15], &[]) }.is_none());
        assert!(unsafe { call_thunk(sum8 as *const () as *const c_void, &[0], &[0.0; 9]) }.is_none());
    }
}
