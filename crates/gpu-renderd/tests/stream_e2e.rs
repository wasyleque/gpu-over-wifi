//! Test od końca do końca na prawdziwym Vulkanie: polecenia zakodowane kodekiem, wykonane przez backend.

use gpu_proto::meta::{find_command, find_struct, StructMeta};
use gpu_proto::msg::PdFunc;
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
        self.meta.members().iter().find(|x| x.name == m).unwrap().offset as usize
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

/// Zakodowane polecenie (kod operacji + parametry) do dopisania do partii.
fn cmd(batch: &mut Vec<u8>, name: &str, args: &[u64]) {
    let (op, c) = find_command(name).unwrap_or_else(|| panic!("brak {name}"));
    assert_eq!(args.len(), c.params().len(), "{name}: liczba parametrów");
    let mut e = Encoder::new();
    unsafe { encode_command(op, args.as_ptr() as *const u8, &mut e, &Identity) }.unwrap_or_else(|er| panic!("{name}: {er:?}"));
    batch.extend(e.finish());
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

const FAMILY_GRAPHICS_OR_COMPUTE: u32 = 0x1 | 0x2 | 0x4;
const SIZE: u64 = 4096;

#[test]
fn fill_buffer_on_the_real_gpu_through_the_command_stream() {
    let Ok(mut b) = VulkanBackend::new() else {
        eprintln!("POMINIĘTO: brak libvulkan");
        return;
    };
    let inst = b.create_instance(0x0040_1000, &[]).unwrap();
    let pd = b.enum_physical_devices(inst).unwrap()[0];

    // Rodzina kolejek z transferem/obliczeniami i typ pamięci widoczny dla hosta (z zapytań M1).
    let count = u32_at(&b.get_physical_device(pd, PdFunc::QueueFamilyProperties2, 0, 0, &[]).unwrap(), 0);
    let qf = b.get_physical_device(pd, PdFunc::QueueFamilyProperties2, 0, count, &[]).unwrap();
    let family = (0..count as usize).find(|i| u32_at(&qf, i * 40 + 16) & FAMILY_GRAPHICS_OR_COMPUTE != 0).unwrap() as u32;
    let mem = b.get_physical_device(pd, PdFunc::MemoryProperties2, 0, 0, &[]).unwrap();
    let types: Vec<(u32, u32)> = (0..u32_at(&mem, 16) as usize).map(|i| (u32_at(&mem, 20 + i * 8), u32_at(&mem, 24 + i * 8))).collect();
    let host_visible = |bits: u32| (0..types.len()).find(|&i| bits & (1 << i) != 0 && types[i].0 & 0x6 == 0x6).map(|i| i as u32);

    let (dev, queue, buf, memory, pool, cb, fence) = (1000u64, 1001u64, 1002u64, 1003u64, 1004u64, 1005u64, 1006u64);
    let prio = [1.0f32];
    let mut qci = S::new("VkDeviceQueueCreateInfo");
    qci.u32("queueFamilyIndex", family).u32("queueCount", 1).ptr("pQueuePriorities", prio.as_ptr());
    let mut dci = S::new("VkDeviceCreateInfo");
    dci.u32("queueCreateInfoCount", 1).u64("pQueueCreateInfos", qci.p());
    let mut bci = S::new("VkBufferCreateInfo");
    bci.u64("size", SIZE).u32("usage", 0x2); // TRANSFER_DST
    let mut batch = Vec::new();
    cmd(&mut batch, "vkCreateDevice", &[pd, dci.p(), 0, &dev as *const u64 as u64]);
    cmd(&mut batch, "vkGetDeviceQueue", &[dev, family as u64, 0, &queue as *const u64 as u64]);
    cmd(&mut batch, "vkCreateBuffer", &[dev, bci.p(), 0, &buf as *const u64 as u64]);
    b.execute(&batch).unwrap();

    // Zapytanie: wymagania pamięci bufora (parametr wyjściowy wraca w odpowiedzi).
    let mut req = S::new("VkMemoryRequirements");
    req.u32("memoryTypeBits", 0xDEAD_BEEF); // śmieci: odpowiedź musi je nadpisać
    let args = [dev, buf, req.p()];
    let (op, c) = find_command("vkGetBufferMemoryRequirements").unwrap();
    let mut e = Encoder::new();
    unsafe { encode_command(op, args.as_ptr() as *const u8, &mut e, &Identity) }.unwrap();
    let out = b.query(&e.finish()).unwrap();
    assert_eq!(u32_at(&out, 0), 0, "kod wyniku");
    unsafe { decode_out_params(c, args.as_ptr() as *const u8, &mut Decoder::new(&out[4..])) }.unwrap();
    let (rsize, bits) = (u64::from_le_bytes(req.bytes()[0..8].try_into().unwrap()), u32_at(req.bytes(), 16));
    assert!(rsize >= SIZE && bits != 0xDEAD_BEEF && bits != 0, "wymagania: size {rsize}, bits {bits:#x}");
    let mt = host_visible(bits).expect("brak typu pamięci widocznego dla hosta");

    let mut ai = S::new("VkMemoryAllocateInfo");
    ai.u64("allocationSize", rsize).u32("memoryTypeIndex", mt);
    let mut pci = S::new("VkCommandPoolCreateInfo");
    pci.u32("queueFamilyIndex", family);
    let mut cai = S::new("VkCommandBufferAllocateInfo");
    cai.u64("commandPool", pool).u32("commandBufferCount", 1);
    let mut cbi = S::new("VkCommandBufferBeginInfo");
    cbi.u32("flags", 1); // ONE_TIME_SUBMIT
    let fci = S::new("VkFenceCreateInfo");
    let mut si = S::new("VkSubmitInfo");
    si.u32("commandBufferCount", 1).ptr("pCommandBuffers", &cb as *const u64);
    let mut batch = Vec::new();
    cmd(&mut batch, "vkAllocateMemory", &[dev, ai.p(), 0, &memory as *const u64 as u64]);
    cmd(&mut batch, "vkBindBufferMemory", &[dev, buf, memory, 0]);
    cmd(&mut batch, "vkCreateCommandPool", &[dev, pci.p(), 0, &pool as *const u64 as u64]);
    cmd(&mut batch, "vkAllocateCommandBuffers", &[dev, cai.p(), &cb as *const u64 as u64]);
    cmd(&mut batch, "vkBeginCommandBuffer", &[cb, cbi.p()]);
    cmd(&mut batch, "vkCmdFillBuffer", &[cb, buf, 0, u64::MAX, 0xCAFE_BABE]);
    cmd(&mut batch, "vkEndCommandBuffer", &[cb]);
    cmd(&mut batch, "vkCreateFence", &[dev, fci.p(), 0, &fence as *const u64 as u64]);
    b.execute(&batch).unwrap();

    // Zerujemy pamięć, żeby odczyt dowodził pracy GPU, a nie zalegających danych.
    b.mem_write(memory, 0, &vec![0u8; SIZE as usize]).unwrap();
    let mut batch = Vec::new();
    cmd(&mut batch, "vkQueueSubmit", &[queue, 1, si.p(), fence]);
    cmd(&mut batch, "vkWaitForFences", &[dev, 1, &fence as *const u64 as u64, 1, u64::MAX]);
    b.execute(&batch).unwrap();

    let data = b.mem_read(memory, 0, SIZE as u32).unwrap();
    assert!(data.chunks(4).all(|w| u32::from_le_bytes(w.try_into().unwrap()) == 0xCAFE_BABE), "GPU nie wypełniło bufora");

    // Zniszczenie obiektów przez strumień i wyrejestrowanie.
    let mut batch = Vec::new();
    cmd(&mut batch, "vkDestroyFence", &[dev, fence, 0]);
    cmd(&mut batch, "vkDestroyBuffer", &[dev, buf, 0]);
    b.execute(&batch).unwrap();
    let mut again = Vec::new();
    cmd(&mut again, "vkDestroyBuffer", &[dev, buf, 0]);
    assert!(b.execute(&again).is_err(), "podwójne zniszczenie: uchwyt już nie istnieje");
    // Zniszczone urządzenie znika z rejestru: ponowne użycie jest błędem, a nie wywołaniem na martwym uchwycie.
    let mut batch = Vec::new();
    cmd(&mut batch, "vkDestroyDevice", &[dev, 0]);
    b.execute(&batch).unwrap();
    let mut idle = Vec::new();
    cmd(&mut idle, "vkDeviceWaitIdle", &[dev]);
    assert!(b.execute(&idle).is_err(), "urządzenie powinno być wyrejestrowane");
    b.destroy_instance(inst).unwrap();
}

#[test]
fn hostile_streams_are_rejected_without_touching_the_driver() {
    let Ok(mut b) = VulkanBackend::new() else { return };
    let inst = b.create_instance(0x0040_1000, &[]).unwrap();
    let _ = b.enum_physical_devices(inst).unwrap();
    // Uchwyt, którego klient nigdy nie utworzył.
    let mut batch = Vec::new();
    cmd(&mut batch, "vkDeviceWaitIdle", &[0xBAD]);
    assert!(b.execute(&batch).is_err());
    // Śmieci i ucięta partia.
    assert!(b.execute(&[0xFF, 0xFF, 1, 2, 3]).is_err());
    assert!(b.execute(&batch[..batch.len() - 1]).is_err());
    // Uchwyt instancji nie jest urządzeniem (rodzaj się nie zgadza).
    let mut batch = Vec::new();
    cmd(&mut batch, "vkDeviceWaitIdle", &[inst]);
    assert!(b.execute(&batch).is_err(), "VkInstance udający VkDevice");
    // Zapis do pamięci, której nie ma.
    assert!(b.mem_write(12345, 0, &[1, 2, 3]).is_err());
    assert!(b.mem_read(12345, 0, 16).is_err());
    b.destroy_instance(inst).unwrap();
}
