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

use crate::handler::ERR_UNKNOWN;
use crate::vk::loader::{LibVulkan, VkInstance};
use crate::vk::registry::{Entry, Registry};

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
        self.reg.wire_to_handle(ty, wire)
    }
}

pub struct Replayer {
    lib: Arc<LibVulkan>,
    pub registry: Registry,
    instance: VkInstance,
    fns: HashMap<u16, *const c_void>,
    maps: HashMap<u64, *mut u8>,
}

// SAFETY: wskaźniki to uchwyty i adresy odwzorowań sterownika używane wyłącznie przez wątek sesji.
unsafe impl Send for Replayer {}

fn code(e: CodecError) -> i32 {
    eprintln!("replay: {e:?}");
    ERR_UNKNOWN
}

impl Replayer {
    pub fn new(lib: Arc<LibVulkan>) -> Replayer {
        Replayer { lib, registry: Registry::new(), instance: std::ptr::null_mut(), fns: HashMap::new(), maps: HashMap::new() }
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
        loop {
            let rec = Recording { reg: &self.registry, seen: RefCell::new(Vec::new()) };
            let Some(dc) = decode_command(&mut dec, &rec).map_err(code)? else { return Ok(()) };
            let seen = rec.seen.into_inner();
            self.run(&dc, &seen, false)?;
        }
    }

    /// Wykonuje jedno polecenie. Odpowiedź: kod wyniku (`i32` LE, także ujemny i dodatni jak `VK_NOT_READY`),
    /// potem zakodowane parametry wyjściowe. Ujemny kod nie rejestruje uchwytów i nie koduje wyników.
    pub fn query(&mut self, data: &[u8]) -> Result<Vec<u8>, i32> {
        let mut dec = Decoder::new(data);
        let rec = Recording { reg: &self.registry, seen: RefCell::new(Vec::new()) };
        let dc = decode_command(&mut dec, &rec).map_err(code)?.ok_or(ERR_UNKNOWN)?;
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
        // SAFETY: `f` to funkcja Vulkana o nazwie polecenia, a parametry odpowiadają jego deklaracji (metadane z vk.xml).
        let rax = unsafe { call_thunk(f, &ints, &floats) }.ok_or(ERR_UNKNOWN)?;
        let result = if cmd.ret == Ret::VkResult { rax as u32 as i32 } else { 0 };
        if result < 0 {
            return if tolerate { Ok(result) } else { Err(result) };
        }
        for ((kind, p, _), ids) in outs.iter().zip(ids) {
            for (i, id) in ids.iter().enumerate() {
                // SAFETY: sterownik wpisał prawdziwy uchwyt w miejsce identyfikatora klienta.
                let real = unsafe { std::ptr::read_unaligned(p.add(i)) };
                self.registry.register(*id, Entry { kind, real, parent, size }).map_err(code)?;
            }
        }
        self.unregister_after(cmd, seen);
        Ok(result)
    }

    /// `vkDestroy*` i `vkFree*` zdejmują obiekt z rejestru (identyfikatory z sieci, nie prawdziwe uchwyty).
    fn unregister_after(&mut self, cmd: &CommandMeta, seen: &[u64]) {
        let n = cmd.name;
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
        self.range_call("vkFlushMappedMemoryRanges", device, e.real)
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
                    call_thunk(destroy, &[e.real, 0], &[]);
                }
            }
            self.registry.unregister(id);
        }
        self.maps.clear();
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
