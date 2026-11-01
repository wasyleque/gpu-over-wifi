//! Minimalne FFI do CUDA Driver API (`libcuda.so.1`, przez `dlopen`): kontekst, pamięć urządzenia i kopia z hosta.
//! Deklaracje wg publicznej dokumentacji CUDA Driver API (typy: `CUdevice` = int, `CUcontext` = wskaźnik, `CUdeviceptr` = u64).

use std::ffi::c_void;

use crate::dl::Lib;

pub type CuResult = i32;
pub type CuContext = *mut c_void;

type FnInit = unsafe extern "C" fn(u32) -> CuResult;
type FnDeviceGet = unsafe extern "C" fn(*mut i32, i32) -> CuResult;
type FnCtxCreate = unsafe extern "C" fn(*mut CuContext, u32, i32) -> CuResult;
type FnCtxDestroy = unsafe extern "C" fn(CuContext) -> CuResult;
type FnCtxSet = unsafe extern "C" fn(CuContext) -> CuResult;
type FnMemAlloc = unsafe extern "C" fn(*mut u64, usize) -> CuResult;
type FnMemFree = unsafe extern "C" fn(u64) -> CuResult;
type FnMemcpyHtoD = unsafe extern "C" fn(u64, *const c_void, usize) -> CuResult;

pub struct Cuda {
    init: FnInit,
    device_get: FnDeviceGet,
    ctx_create: FnCtxCreate,
    ctx_destroy: FnCtxDestroy,
    ctx_set: FnCtxSet,
    mem_alloc: FnMemAlloc,
    mem_free: FnMemFree,
    memcpy_htod: FnMemcpyHtoD,
}

fn ck(what: &str, r: CuResult) -> Result<(), String> {
    if r == 0 {
        Ok(())
    } else {
        Err(format!("{what}: kod CUDA {r}"))
    }
}

macro_rules! sym {
    ($lib:expr, $name:literal, $ty:ty) => {{
        let p = $lib.sym($name).ok_or_else(|| format!("libcuda: brak symbolu {}", $name))?;
        // SAFETY: symbol ma sygnaturę z dokumentacji CUDA Driver API.
        unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
    }};
}

impl Cuda {
    pub fn load() -> Result<Cuda, String> {
        let lib = Lib::open(&["libcuda.so.1", "libcuda.so"])?;
        Ok(Cuda {
            init: sym!(lib, "cuInit", FnInit),
            device_get: sym!(lib, "cuDeviceGet", FnDeviceGet),
            ctx_create: sym!(lib, "cuCtxCreate_v2", FnCtxCreate),
            ctx_destroy: sym!(lib, "cuCtxDestroy_v2", FnCtxDestroy),
            ctx_set: sym!(lib, "cuCtxSetCurrent", FnCtxSet),
            mem_alloc: sym!(lib, "cuMemAlloc_v2", FnMemAlloc),
            mem_free: sym!(lib, "cuMemFree_v2", FnMemFree),
            memcpy_htod: sym!(lib, "cuMemcpyHtoD_v2", FnMemcpyHtoD),
        })
    }

    /// Inicjuje CUDA i tworzy kontekst na urządzeniu `ordinal`.
    pub fn create_context(&self, ordinal: i32) -> Result<CuContext, String> {
        let mut dev = 0i32;
        let mut ctx: CuContext = std::ptr::null_mut();
        // SAFETY: poprawne wskaźniki wyjściowe; sygnatury z dokumentacji.
        unsafe {
            ck("cuInit", (self.init)(0))?;
            ck("cuDeviceGet", (self.device_get)(&mut dev, ordinal))?;
            ck("cuCtxCreate", (self.ctx_create)(&mut ctx, 0, dev))?;
        }
        Ok(ctx)
    }

    pub fn destroy_context(&self, ctx: CuContext) {
        // SAFETY: kontekst pochodzi z `create_context`.
        unsafe { (self.ctx_destroy)(ctx) };
    }

    /// Ustawia kontekst jako bieżący dla tego wątku (wymagane przed wywołaniami CUDA/NVENC z innego wątku).
    pub fn set_current(&self, ctx: CuContext) -> Result<(), String> {
        // SAFETY: kontekst jest ważny.
        ck("cuCtxSetCurrent", unsafe { (self.ctx_set)(ctx) })
    }

    pub fn alloc(&self, bytes: usize) -> Result<u64, String> {
        let mut p = 0u64;
        // SAFETY: wskaźnik wyjściowy jest poprawny.
        ck("cuMemAlloc", unsafe { (self.mem_alloc)(&mut p, bytes) })?;
        Ok(p)
    }

    pub fn free(&self, ptr: u64) {
        // SAFETY: `ptr` pochodzi z `alloc`.
        unsafe { (self.mem_free)(ptr) };
    }

    /// Kopiuje bajty z hosta do pamięci urządzenia (`dst` musi mieć co najmniej `src.len()` bajtów).
    pub fn copy_to_device(&self, dst: u64, src: &[u8]) -> Result<(), String> {
        // SAFETY: `src` jest poprawnym wycinkiem; rozmiar alokacji pilnuje wołający.
        ck("cuMemcpyHtoD", unsafe { (self.memcpy_htod)(dst, src.as_ptr() as *const c_void, src.len()) })
    }
}
