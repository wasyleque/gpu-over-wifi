//! Sonda (uruchamiana ręcznie na serwerze z prawdziwym sterownikiem NVIDIA): jak wyglądają bajty deskryptora bufora zwracane przez
//! `vkGetDescriptorEXT` w zależności od (adres, zakres, format)? Jeśli to prosta funkcja bitowa wejścia, klient może je liczyć lokalnie
//! zamiast pytać serwer (90% obiegów po sieci w Cyberpunku to właśnie te zapytania).
//!
//! `cargo test -p gpu-renderd --test descriptor_probe -- --ignored --nocapture` wypisuje linie `PROBE typ adres zakres format bajty`.

use gpu_proto::meta::{find_command, find_struct, StructMeta};
use gpu_proto::stype::STYPE_TABLE;
use gpu_renderd::handler::GpuBackend;
use gpu_renderd::vk::backend::VulkanBackend;
use gpu_vkcodec::codec::{decode_out_params, encode_command, Identity};
use gpu_wire::{Decoder, Encoder};

struct S {
    meta: &'static StructMeta,
    w: Vec<u64>,
}

impl S {
    fn new(name: &str) -> S {
        let meta = find_struct(name).unwrap();
        let mut s = S { meta, w: vec![0; (meta.size as usize).div_ceil(8).max(1)] };
        if let Some(e) = STYPE_TABLE.iter().find(|e| e.name == name) {
            s.u32("sType", e.stype);
        }
        s
    }
    fn off(&self, m: &str) -> usize {
        self.meta.members().iter().find(|x| x.name == m).unwrap_or_else(|| panic!("brak członka {m}")).offset as usize
    }
    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.w.as_mut_ptr() as *mut u8, self.w.len() * 8) }
    }
    fn u32(&mut self, m: &str, v: u32) -> &mut S {
        let o = self.off(m);
        self.bytes()[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, m: &str, v: u64) -> &mut S {
        let o = self.off(m);
        self.bytes()[o..o + 8].copy_from_slice(&v.to_le_bytes());
        self
    }
    fn ptr<T>(&mut self, m: &str, p: *const T) -> &mut S {
        self.u64(m, p as usize as u64)
    }
    fn p(&self) -> u64 {
        self.w.as_ptr() as u64
    }
}

fn cmd(batch: &mut Vec<u8>, name: &str, args: &[u64]) {
    let (op, c) = find_command(name).unwrap_or_else(|| panic!("brak {name}"));
    assert_eq!(args.len(), c.params().len(), "{name}: liczba parametrów");
    let mut e = Encoder::new();
    unsafe { encode_command(op, args.as_ptr() as *const u8, &mut e, &Identity) }.unwrap_or_else(|er| panic!("{name}: {er:?}"));
    batch.extend(e.finish());
}

/// Wywołuje `vkGetDescriptorEXT` dla bufora (adres, zakres, format) i zwraca bajty opisu (`size` bajtów, wypełnienie 0xAA wykrywa niezapisane).
fn get_descriptor(b: &mut VulkanBackend, dev: u64, ty: u32, address: u64, range: u64, format: u32, size: usize) -> Vec<u8> {
    let mut addr = S::new("VkDescriptorAddressInfoEXT");
    addr.u64("address", address).u64("range", range).u32("format", format);
    let mut info = S::new("VkDescriptorGetInfoEXT");
    info.u32("type", ty).u64("data", addr.p());
    let mut out = vec![0xAAu8; size];
    let args = [dev, info.p(), size as u64, out.as_mut_ptr() as u64];
    let (op, c) = find_command("vkGetDescriptorEXT").unwrap();
    let mut e = Encoder::new();
    unsafe { encode_command(op, args.as_ptr() as *const u8, &mut e, &Identity) }.unwrap();
    let resp = b.query(&e.finish()).unwrap();
    assert_eq!(i32::from_le_bytes(resp[0..4].try_into().unwrap()), 0, "wynik polecenia");
    unsafe { decode_out_params(c, args.as_ptr() as *const u8, &mut Decoder::new(&resp[4..])) }.unwrap();
    out
}

#[test]
#[ignore = "wymaga prawdziwej karty NVIDIA z VK_EXT_descriptor_buffer; uruchamiać ręcznie na serwerze"]
fn probe_buffer_descriptor_encoding() {
    let Ok(mut b) = VulkanBackend::new() else {
        eprintln!("brak libvulkan");
        return;
    };
    let inst = b.create_instance(0x0040_3000, &[]).unwrap();
    let pds = b.enum_physical_devices(inst).unwrap();
    let pd = pds[0];

    // Urządzenie z VK_EXT_descriptor_buffer i włączoną cechą descriptorBuffer.
    let exts = ["VK_EXT_descriptor_buffer", "VK_KHR_buffer_device_address", "VK_KHR_synchronization2"];
    let cstrs: Vec<std::ffi::CString> = exts.iter().map(|e| std::ffi::CString::new(*e).unwrap()).collect();
    let ptrs: Vec<*const std::os::raw::c_char> = cstrs.iter().map(|c| c.as_ptr()).collect();
    let mut feat = S::new("VkPhysicalDeviceDescriptorBufferFeaturesEXT");
    feat.u32("descriptorBuffer", 1);
    let prio = [1.0f32];
    let mut qci = S::new("VkDeviceQueueCreateInfo");
    qci.u32("queueFamilyIndex", 0).u32("queueCount", 1).ptr("pQueuePriorities", prio.as_ptr());
    let mut dci = S::new("VkDeviceCreateInfo");
    dci.u64("pNext", feat.p()).u32("queueCreateInfoCount", 1).u64("pQueueCreateInfos", qci.p());
    dci.u32("enabledExtensionCount", ptrs.len() as u32).u64("ppEnabledExtensionNames", ptrs.as_ptr() as u64);
    let dev = 1000u64;
    let mut batch = Vec::new();
    cmd(&mut batch, "vkCreateDevice", &[pd, dci.p(), 0, &dev as *const u64 as u64]);
    b.execute(&batch).unwrap();

    // Rozmiar deskryptora (zmierzony wypełnieniem 0xAA): UNIFORM_BUFFER 8 B, STORAGE_BUFFER 16 B na tej karcie.
    let size_of_type = |ty: u32| if ty == 6 { 8usize } else { 16 };
    let hex = |d: &[u8]| d.iter().map(|x| format!("{x:02x}")).collect::<String>();
    // Wejścia: baza, pojedyncze bity adresu (0..48), bity zakresu (0..32), formaty, losowe pary.
    let mut cases: Vec<(u64, u64, u32)> = vec![(0, 0, 0), (0, 1, 0), (0x1000, 256, 0)];
    for i in 0..64 {
        cases.push((1u64 << i, 0, 0));
    }
    for i in 0..40 {
        cases.push((0, 1u64 << i, 0));
    }
    for f in [1u32, 2, 3, 37, 44, 97, 109] {
        cases.push((0x1000, 256, f));
    }
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..200 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let address = (x & 0x0000_7FFF_FFFF_FF00) | (x >> 60) << 2;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let range = (x & 0xFF_FFFF) + 1;
        cases.push((address, range, 0));
    }
    // Przemiatania (UNIFORM_BUFFER): zakres 1..=1100 przy stałym adresie oraz adres +0..+520 przy stałym zakresie.
    for r in 1..=1100u64 {
        let d = get_descriptor(&mut b, dev, 6, 0x7000_0000_1000, r, 0, 8);
        println!("SWEEPR {r} {}", hex(&d));
    }
    for k in 0..=520u64 {
        let d = get_descriptor(&mut b, dev, 6, 0x7000_0000_1000 + k, 0x1000, 0, 8);
        println!("SWEEPA {k} {}", hex(&d));
    }
    for ty in [6u32, 7] {
        for &(a, r, f) in &cases {
            let d = get_descriptor(&mut b, dev, ty, a, r, f, size_of_type(ty));
            println!("PROBE {ty} {a:#x} {r:#x} {f} {}", hex(&d));
        }
    }
}
