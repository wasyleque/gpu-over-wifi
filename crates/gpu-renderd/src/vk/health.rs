//! Sprawdzenie zdrowia karty przy starcie: po `VK_ERROR_DEVICE_LOST` (Xid 32) sterownik NVIDIA bywa niezdolny do `vkCreateDevice` jeszcze przez
//! kilkadziesiąt sekund po restarcie procesu; klienci łączyli się wtedy z demonem, który przyjmował sesje, ale zwracał -3 (gra wisiała na ekranie
//! startowym). Demon próbuje więc najpierw założyć i zniszczyć jedno urządzenie; gdy się nie da (albo wisi dłużej niż limit), kończy się kodem 75
//! PRZED nasłuchiwaniem, więc klient dostaje odmowę połączenia zamiast zawieszenia, a nadzorca (systemd) ponawia próbę.
//! `GPU_RENDERD_PROBE=0` wyłącza.

use super::loader::{GlobalFns, LibVulkan, VkInstance, VkPhysicalDevice};
use std::ffi::{c_void, CStr};

type FnCreateDevice = unsafe extern "C" fn(VkPhysicalDevice, *const c_void, *const c_void, *mut *mut c_void) -> i32;
type FnDestroyDevice = unsafe extern "C" fn(*mut c_void, *const c_void);

/// Zakłada i niszczy jedno urządzenie na pierwszym fizycznym urządzeniu (rodzina kolejek 0, bez rozszerzeń). `Ok` = karta działa.
pub fn probe_device() -> Result<(), i32> {
    let lib = LibVulkan::load().map_err(|_| -3)?;
    let global = GlobalFns::load(&lib).map_err(|_| -3)?;
    let _g = super::loader::GLOBAL_CALLS.lock().unwrap_or_else(|e| e.into_inner());
    let mut info = vec![0u64; 8]; // VkInstanceCreateInfo: sType = 1, reszta zera
    info[0] = 1;
    let mut inst: VkInstance = std::ptr::null_mut();
    // SAFETY: minimalna poprawna VkInstanceCreateInfo; wynik niszczymy na końcu.
    let r = unsafe { (global.create_instance)(info.as_ptr() as *const c_void, std::ptr::null(), &mut inst) };
    if r < 0 {
        return Err(r);
    }
    let result = (|| {
        // SAFETY: `inst` jest ważną instancją; nazwy funkcji są ze specyfikacji.
        let fns = unsafe { super::loader::InstanceFns::load(&lib, inst) }.map_err(|_| -3)?;
        let mut n = 0u32;
        let r = unsafe { (fns.enumerate_physical_devices)(inst, &mut n, std::ptr::null_mut()) };
        if r < 0 || n == 0 {
            return Err(if r < 0 { r } else { -3 });
        }
        let mut pd: Vec<VkPhysicalDevice> = vec![std::ptr::null_mut(); n as usize];
        let r = unsafe { (fns.enumerate_physical_devices)(inst, &mut n, pd.as_mut_ptr()) };
        if r < 0 {
            return Err(r);
        }
        let create = unsafe { lib.proc_addr(inst, CStr::from_bytes_with_nul(b"vkCreateDevice\0").map_err(|_| -3)?) }.ok_or(-3)?;
        let destroy = unsafe { lib.proc_addr(inst, CStr::from_bytes_with_nul(b"vkDestroyDevice\0").map_err(|_| -3)?) }.ok_or(-3)?;
        // SAFETY: typy odpowiadają sygnaturom ze specyfikacji Vulkana.
        let (create, destroy) = unsafe { (std::mem::transmute::<*const c_void, FnCreateDevice>(create), std::mem::transmute::<*const c_void, FnDestroyDevice>(destroy)) };
        let prio = [1.0f32];
        // VkDeviceQueueCreateInfo (40 B): sType=2 @0, queueFamilyIndex @20, queueCount @24, pQueuePriorities @32.
        let mut q = vec![0u8; 40];
        q[0..4].copy_from_slice(&2u32.to_le_bytes());
        q[24..28].copy_from_slice(&1u32.to_le_bytes());
        q[32..40].copy_from_slice(&(prio.as_ptr() as u64).to_le_bytes());
        // VkDeviceCreateInfo (72 B): sType=3 @0, queueCreateInfoCount @20, pQueueCreateInfos @24.
        let mut d = vec![0u8; 72];
        d[0..4].copy_from_slice(&3u32.to_le_bytes());
        d[20..24].copy_from_slice(&1u32.to_le_bytes());
        d[24..32].copy_from_slice(&(q.as_ptr() as u64).to_le_bytes());
        let mut ok = true;
        let mut last = -3;
        // Wszystkie widoczne urządzenia muszą się założyć (llvmpipe zawsze się zakłada, więc jedno udane nie dowodzi, że karta działa).
        for &p in &pd {
            let mut dev: *mut c_void = std::ptr::null_mut();
            let r = unsafe { create(p, d.as_ptr() as *const c_void, std::ptr::null(), &mut dev) };
            if r >= 0 && !dev.is_null() {
                unsafe { destroy(dev, std::ptr::null()) };
            } else {
                ok = false;
                last = r;
            }
        }
        if ok { Ok(()) } else { Err(last) }
    })();
    // SAFETY: instancja z `create_instance` powyżej, urządzenia zniszczone.
    if let Some(f) = unsafe { lib.proc_addr(inst, CStr::from_bytes_with_nul(b"vkDestroyInstance\0").unwrap_or(c"")) } {
        let destroy_instance: unsafe extern "C" fn(VkInstance, *const c_void) = unsafe { std::mem::transmute(f) };
        unsafe { destroy_instance(inst, std::ptr::null()) };
    }
    result
}

/// Wołane z `main` przed nasłuchiwaniem: do `limit` sekund na udane założenie urządzenia, inaczej `exit(75)`.
pub fn gate_or_exit(limit: std::time::Duration) {
    if std::env::var("GPU_RENDERD_PROBE").map(|v| v == "0").unwrap_or(false) {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("gpu-probe".into()).spawn(move || {
        let _ = tx.send(probe_device());
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(())) => eprintln!("gpu-renderd: sprawdzenie karty (vkCreateDevice) przeszło"),
        Ok(Err(e)) => {
            eprintln!("gpu-renderd: karta nie zakłada urządzenia (VkResult {e}); koniec (75), nadzorca ponowi, klienci dostają odmowę połączenia");
            std::process::exit(75);
        }
        Err(_) => {
            eprintln!("gpu-renderd: vkCreateDevice wisi ponad {} s (sterownik po DEVICE_LOST); koniec (75)", limit.as_secs());
            std::process::exit(75);
        }
    }
}
