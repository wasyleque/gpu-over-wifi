//! Zestawy deskryptorów zwalniane przez `vkResetDescriptorPool` / `vkDestroyDescriptorPool` muszą znikać z rejestru uchwytów serwera:
//! ATS alokuje ponad milion zestawów w jednej sesji, a rejestr ma limit (`MAX_HANDLES`), po którym rejestracja zwraca `Handle(..)`
//! i gra się kończy. Test na prawdziwym Vulkanie (pomijany bez libvulkan / karty).

use gpu_proto::meta::{find_command, find_struct, StructMeta};
use gpu_proto::stype::STYPE_TABLE;
use gpu_renderd::handler::GpuBackend;
use gpu_renderd::vk::backend::VulkanBackend;
use gpu_vkcodec::codec::{encode_command, Identity};
use gpu_wire::Encoder;

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

#[test]
fn reset_and_destroy_of_a_descriptor_pool_release_its_sets_from_the_registry() {
    let Ok(mut b) = VulkanBackend::new() else {
        eprintln!("brak libvulkan");
        return;
    };
    let Ok(inst) = b.create_instance(0x0040_3000, &[]) else {
        eprintln!("POMINIĘTO: brak instancji Vulkana");
        return;
    };
    let Some(&pd) = b.enum_physical_devices(inst).ok().and_then(|p| p.first().copied()).as_ref() else {
        eprintln!("POMINIĘTO: brak karty");
        return;
    };
    let prio = [1.0f32];
    let mut qci = S::new("VkDeviceQueueCreateInfo");
    qci.u32("queueFamilyIndex", 0).u32("queueCount", 1).u64("pQueuePriorities", prio.as_ptr() as u64);
    let mut dci = S::new("VkDeviceCreateInfo");
    dci.u32("queueCreateInfoCount", 1).u64("pQueueCreateInfos", qci.p());
    let (dev, layout, pool) = (1000u64, 1001u64, 1002u64);
    let mut batch = Vec::new();
    cmd(&mut batch, "vkCreateDevice", &[pd, dci.p(), 0, &dev as *const u64 as u64]);
    // Układ z jednym UNIFORM_BUFFER (typ 6) i pula na 8 zestawów.
    let mut binding = [0u32; 6]; // VkDescriptorSetLayoutBinding: binding, type, count, stageFlags, pImmutableSamplers (8 B)
    binding[1] = 6;
    binding[2] = 1;
    binding[3] = 0x1f;
    let mut lci = S::new("VkDescriptorSetLayoutCreateInfo");
    lci.u32("bindingCount", 1).u64("pBindings", binding.as_ptr() as u64);
    cmd(&mut batch, "vkCreateDescriptorSetLayout", &[dev, lci.p(), 0, &layout as *const u64 as u64]);
    let size = [6u32, 64u32];
    let mut pci = S::new("VkDescriptorPoolCreateInfo");
    pci.u32("maxSets", 8).u32("poolSizeCount", 1).u64("pPoolSizes", size.as_ptr() as u64);
    cmd(&mut batch, "vkCreateDescriptorPool", &[dev, pci.p(), 0, &pool as *const u64 as u64]);
    if b.execute(&batch).is_err() {
        eprintln!("POMINIĘTO: sterownik nie utworzył urządzenia, układu lub puli");
        return;
    }
    let base = b.registered_handles();
    let layouts = [layout; 4];
    let mut next = 5000u64;
    for round in 0..300 {
        let mut ids = [0u64; 4];
        for id in &mut ids {
            next += 1;
            *id = next;
        }
        let mut ai = S::new("VkDescriptorSetAllocateInfo");
        ai.u64("descriptorPool", pool).u32("descriptorSetCount", 4).u64("pSetLayouts", layouts.as_ptr() as u64);
        let mut batch = Vec::new();
        cmd(&mut batch, "vkAllocateDescriptorSets", &[dev, ai.p(), ids.as_ptr() as u64]);
        b.execute(&batch).unwrap_or_else(|e| panic!("alokacja w rundzie {round}: {e}"));
        assert_eq!(b.registered_handles(), base + 4, "rundа {round}: zestawy zarejestrowane");
        let mut batch = Vec::new();
        cmd(&mut batch, "vkResetDescriptorPool", &[dev, pool, 0]);
        b.execute(&batch).unwrap();
        assert_eq!(b.registered_handles(), base, "runda {round}: po resecie puli rejestr wraca do stanu wyjściowego");
    }
    // Zniszczenie puli też zdejmuje jej zestawy (i samą pulę).
    let mut ids = [0u64; 4];
    for id in &mut ids {
        next += 1;
        *id = next;
    }
    let mut ai = S::new("VkDescriptorSetAllocateInfo");
    ai.u64("descriptorPool", pool).u32("descriptorSetCount", 4).u64("pSetLayouts", layouts.as_ptr() as u64);
    let mut batch = Vec::new();
    cmd(&mut batch, "vkAllocateDescriptorSets", &[dev, ai.p(), ids.as_ptr() as u64]);
    cmd(&mut batch, "vkDestroyDescriptorPool", &[dev, pool, 0]);
    b.execute(&batch).unwrap();
    assert_eq!(b.registered_handles(), base - 1, "po zniszczeniu puli znikają zestawy i pula");
    eprintln!("OK: 300 rund alokacji i resetów, rejestr stały ({base} wpisów), zniszczenie puli sprząta zestawy");
}
