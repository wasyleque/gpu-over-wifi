use gpu_proto::meta::{find_struct, StructMeta};
use gpu_proto::stype::STYPE_TABLE;
use gpu_vkcodec::codec::{decode_struct, encode_struct, Ctx, Identity, MAX_CHAIN};
use gpu_vkcodec::CodecError;
use gpu_wire::{Decoder, Encoder};

/// Struktura w układzie C zbudowana ręcznie na podstawie metadanych.
struct C {
    meta: &'static StructMeta,
    words: Vec<u64>,
}

fn st(name: &str) -> u32 {
    STYPE_TABLE.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("brak {name}")).stype
}

impl C {
    fn new(name: &str) -> C {
        let meta = find_struct(name).unwrap();
        let mut c = C { meta, words: vec![0u64; (meta.size as usize).div_ceil(8).max(1)] };
        if let Some(e) = STYPE_TABLE.iter().find(|e| e.name == name) {
            c.set_u32("sType", e.stype);
        }
        c
    }
    fn off(&self, member: &str) -> usize {
        self.meta.members().iter().find(|m| m.name == member).unwrap_or_else(|| panic!("brak {member}")).offset as usize
    }
    fn ptr(&self) -> *const u8 {
        self.words.as_ptr() as *const u8
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr() as *mut u8, self.words.len() * 8) }
    }
    fn set_u32(&mut self, member: &str, v: u32) {
        let o = self.off(member);
        self.bytes_mut()[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn set_u64(&mut self, member: &str, v: u64) {
        let o = self.off(member);
        self.bytes_mut()[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn set_ptr<T>(&mut self, member: &str, p: *const T) {
        self.set_u64(member, p as usize as u64);
    }
}

fn encode(c: &C) -> Vec<u8> {
    let mut e = Encoder::new();
    unsafe { encode_struct(c.meta, c.ptr(), &mut e, &Identity) }.unwrap();
    e.finish()
}

unsafe fn rd_u32(p: *const u8, off: usize) -> u32 {
    std::ptr::read_unaligned(p.add(off) as *const u32)
}

unsafe fn rd_ptr(p: *const u8, off: usize) -> *const u8 {
    std::ptr::read_unaligned(p.add(off) as *const *const u8)
}

unsafe fn cstr(p: *const u8) -> String {
    std::ffi::CStr::from_ptr(p as *const std::ffi::c_char).to_string_lossy().into_owned()
}

fn off(name: &str, member: &str) -> usize {
    find_struct(name).unwrap().members().iter().find(|m| m.name == member).unwrap().offset as usize
}

#[test]
fn instance_create_info_with_app_info_and_extension_names_roundtrips() {
    let mut app = C::new("VkApplicationInfo");
    let name = std::ffi::CString::new("cube").unwrap();
    app.set_ptr("pApplicationName", name.as_ptr());
    app.set_u32("apiVersion", 0x0040_3000);
    let e1 = std::ffi::CString::new("VK_KHR_surface").unwrap();
    let e2 = std::ffi::CString::new("VK_KHR_xcb_surface").unwrap();
    let names = [e1.as_ptr(), e2.as_ptr()];
    let mut ci = C::new("VkInstanceCreateInfo");
    ci.set_ptr("pApplicationInfo", app.ptr());
    ci.set_u32("enabledExtensionCount", 2);
    ci.set_ptr("ppEnabledExtensionNames", names.as_ptr());
    let wire = encode(&ci);
    let d = decode_struct(ci.meta, &mut Decoder::new(&wire), &Identity).unwrap();
    unsafe {
        assert_eq!(rd_u32(d.ptr, 0), 1);
        let a = rd_ptr(d.ptr, off("VkInstanceCreateInfo", "pApplicationInfo"));
        assert!(!a.is_null() && a != app.ptr(), "nowa kopia w arenie, nie wskaźnik klienta");
        assert_eq!(rd_u32(a, off("VkApplicationInfo", "apiVersion")), 0x0040_3000);
        assert_eq!(cstr(rd_ptr(a, off("VkApplicationInfo", "pApplicationName"))), "cube");
        assert!(rd_ptr(a, off("VkApplicationInfo", "pEngineName")).is_null());
        let arr = rd_ptr(d.ptr, off("VkInstanceCreateInfo", "ppEnabledExtensionNames"));
        assert_eq!(cstr(rd_ptr(arr, 0)), "VK_KHR_surface");
        assert_eq!(cstr(rd_ptr(arr, 8)), "VK_KHR_xcb_surface");
        assert!(rd_ptr(d.ptr, off("VkInstanceCreateInfo", "ppEnabledLayerNames")).is_null());
    }
    assert_eq!(wire, { let mut e = Encoder::new(); unsafe { encode_struct(ci.meta, d.ptr, &mut e, &Identity) }.unwrap(); e.finish() }, "encode(decode(x)) == x");
}

#[test]
fn shader_module_code_uses_latexmath_length() {
    let code = [0x0723_0203u32, 1, 2, 3];
    let mut sm = C::new("VkShaderModuleCreateInfo");
    sm.set_u64("codeSize", 16);
    sm.set_ptr("pCode", code.as_ptr());
    let wire = encode(&sm);
    let d = decode_struct(sm.meta, &mut Decoder::new(&wire), &Identity).unwrap();
    unsafe {
        let p = rd_ptr(d.ptr, off("VkShaderModuleCreateInfo", "pCode"));
        assert_eq!((0..4).map(|i| rd_u32(p, i * 4)).collect::<Vec<_>>(), code.to_vec());
    }
}

#[test]
fn pnext_chain_is_rebuilt_with_real_links_and_forced_stypes() {
    let mut v12 = C::new("VkPhysicalDeviceVulkan12Features");
    v12.set_u32("bufferDeviceAddress", 1);
    let mut v11 = C::new("VkPhysicalDeviceVulkan11Features");
    v11.set_u32("multiview", 1);
    v12.set_ptr("pNext", v11.ptr());
    let mut f2 = C::new("VkPhysicalDeviceFeatures2");
    f2.set_ptr("pNext", v12.ptr());
    let wire = encode(&f2);
    let d = decode_struct(f2.meta, &mut Decoder::new(&wire), &Identity).unwrap();
    unsafe {
        let n1 = rd_ptr(d.ptr, 8);
        assert_eq!(rd_u32(n1, 0), st("VkPhysicalDeviceVulkan12Features"));
        assert_eq!(rd_u32(n1, off("VkPhysicalDeviceVulkan12Features", "bufferDeviceAddress")), 1);
        let n2 = rd_ptr(n1, 8);
        assert_eq!(rd_u32(n2, 0), st("VkPhysicalDeviceVulkan11Features"));
        assert_eq!(rd_u32(n2, off("VkPhysicalDeviceVulkan11Features", "multiview")), 1);
        assert!(rd_ptr(n2, 8).is_null());
    }
}

#[test]
fn unknown_and_disallowed_structures_in_the_chain_are_skipped_by_the_encoder() {
    struct OnlyV11;
    impl Ctx for OnlyV11 {
        fn handle_to_wire(&self, _: &str, r: u64) -> Result<u64, CodecError> { Ok(r) }
        fn wire_to_handle(&self, _: &str, w: u64) -> Result<u64, CodecError> { Ok(w) }
        fn allow_stype(&self, s: u32) -> bool { s == st("VkPhysicalDeviceVulkan11Features") }
    }
    let mut unknown = C::new("VkPhysicalDeviceVulkan12Features");
    unknown.set_u32("sType", 0x7FFF_0000); // nieznany sType
    let mut v11 = C::new("VkPhysicalDeviceVulkan11Features");
    let mut v12 = C::new("VkPhysicalDeviceVulkan12Features");
    v12.set_ptr("pNext", unknown.ptr());
    v11.set_ptr("pNext", v12.ptr());
    let mut f2 = C::new("VkPhysicalDeviceFeatures2");
    f2.set_ptr("pNext", v11.ptr());
    let mut e = Encoder::new();
    unsafe { encode_struct(f2.meta, f2.ptr(), &mut e, &OnlyV11) }.unwrap();
    let wire = e.finish();
    let d = decode_struct(f2.meta, &mut Decoder::new(&wire), &OnlyV11).unwrap();
    unsafe {
        let n1 = rd_ptr(d.ptr, 8);
        assert_eq!(rd_u32(n1, 0), st("VkPhysicalDeviceVulkan11Features"));
        assert!(rd_ptr(n1, 8).is_null(), "pominięte: Vulkan12 (niedozwolony) i nieznany sType");
    }
    // Serwer z tą samą polityką odrzuca sType spoza listy, jeśli klient go jednak wyśle.
    let mut full = Encoder::new();
    unsafe { encode_struct(f2.meta, f2.ptr(), &mut full, &Identity) }.unwrap();
    assert!(matches!(decode_struct(f2.meta, &mut Decoder::new(&full.finish()), &OnlyV11), Err(CodecError::UnknownStype(_))));
}

#[test]
fn device_create_info_with_queue_arrays_and_features_chain() {
    let prio = [1.0f32, 0.5];
    let mut q = [C::new("VkDeviceQueueCreateInfo"), C::new("VkDeviceQueueCreateInfo")];
    q[0].set_u32("queueFamilyIndex", 0);
    q[0].set_u32("queueCount", 2);
    q[0].set_ptr("pQueuePriorities", prio.as_ptr());
    q[1].set_u32("queueFamilyIndex", 3);
    q[1].set_u32("queueCount", 1);
    q[1].set_ptr("pQueuePriorities", prio.as_ptr());
    // Tablica struktur musi leżeć ciągle w pamięci.
    let size = q[0].meta.size as usize;
    let mut flat = vec![0u64; (size * 2).div_ceil(8)];
    let fb = unsafe { std::slice::from_raw_parts_mut(flat.as_mut_ptr() as *mut u8, size * 2) };
    fb[..size].copy_from_slice(unsafe { std::slice::from_raw_parts(q[0].ptr(), size) });
    fb[size..].copy_from_slice(unsafe { std::slice::from_raw_parts(q[1].ptr(), size) });
    let ext = std::ffi::CString::new("VK_KHR_swapchain").unwrap();
    let exts = [ext.as_ptr()];
    let mut f2 = C::new("VkPhysicalDeviceFeatures2");
    let mut dci = C::new("VkDeviceCreateInfo");
    dci.set_ptr("pNext", f2.ptr());
    dci.set_u32("queueCreateInfoCount", 2);
    dci.set_ptr("pQueueCreateInfos", flat.as_ptr());
    dci.set_u32("enabledExtensionCount", 1);
    dci.set_ptr("ppEnabledExtensionNames", exts.as_ptr());
    let _ = &mut f2;
    let wire = encode(&dci);
    let d = decode_struct(dci.meta, &mut Decoder::new(&wire), &Identity).unwrap();
    unsafe {
        let arr = rd_ptr(d.ptr, off("VkDeviceCreateInfo", "pQueueCreateInfos"));
        assert_eq!(rd_u32(arr, off("VkDeviceQueueCreateInfo", "queueFamilyIndex") + size), 3, "drugi element tablicy");
        let pr = rd_ptr(arr, off("VkDeviceQueueCreateInfo", "pQueuePriorities")) as *const f32;
        assert_eq!((*pr, *pr.add(1)), (1.0, 0.5));
        let f = rd_ptr(d.ptr, 8);
        assert_eq!(rd_u32(f, 0), st("VkPhysicalDeviceFeatures2"));
    }
}

#[test]
fn handles_are_translated_in_both_directions() {
    struct Plus;
    impl Ctx for Plus {
        fn handle_to_wire(&self, _: &str, r: u64) -> Result<u64, CodecError> { Ok(r + 1000) }
        fn wire_to_handle(&self, _: &str, w: u64) -> Result<u64, CodecError> { w.checked_sub(500).ok_or(CodecError::Handle(w)) }
    }
    let mut b = C::new("VkBindBufferMemoryInfo");
    b.set_u64("buffer", 7);
    b.set_u64("memory", 9);
    b.set_u64("memoryOffset", 4096);
    let mut e = Encoder::new();
    unsafe { encode_struct(b.meta, b.ptr(), &mut e, &Plus) }.unwrap();
    let wire = e.finish();
    let d = decode_struct(b.meta, &mut Decoder::new(&wire), &Plus).unwrap();
    unsafe {
        assert_eq!(std::ptr::read_unaligned(d.ptr.add(off("VkBindBufferMemoryInfo", "buffer")) as *const u64), 1007 - 500);
        assert_eq!(std::ptr::read_unaligned(d.ptr.add(off("VkBindBufferMemoryInfo", "memoryOffset")) as *const u64), 4096);
    }
    let mut bad = Encoder::new();
    unsafe { encode_struct(b.meta, b.ptr(), &mut bad, &Identity) }.unwrap(); // handle 7 < 500
    assert!(matches!(decode_struct(b.meta, &mut Decoder::new(&bad.finish()), &Plus), Err(CodecError::Handle(7))));
}

#[test]
fn forged_element_count_is_rejected_so_the_server_never_reads_out_of_bounds() {
    let e1 = std::ffi::CString::new("VK_KHR_surface").unwrap();
    let names = [e1.as_ptr()];
    let mut ci = C::new("VkInstanceCreateInfo");
    ci.set_u32("enabledExtensionCount", 0x0BAD_F00D); // wartość znacznikowa
    ci.set_ptr("ppEnabledExtensionNames", names.as_ptr());
    // Koder uczciwie zakodowałby 0x0BADF00D elementów (limit); budujemy więc poprawny strumień i podmieniamy licznik.
    ci.set_u32("enabledExtensionCount", 1);
    let wire = encode(&ci);
    let ok = decode_struct(ci.meta, &mut Decoder::new(&wire), &Identity);
    assert!(ok.is_ok());
    ci.set_u32("enabledExtensionCount", 0x0BAD_F00D);
    let mut forged = encode_with_count_override(&ci, 1);
    assert!(decode_struct(ci.meta, &mut Decoder::new(&forged), &Identity).is_err(), "licznik 0x0BADF00D przy 1 elemencie");
    forged.clear();
}

/// Koduje strukturę tak, jakby licznik miał wartość `real` (podmieniamy surowe bajty licznika w strumieniu).
fn encode_with_count_override(ci: &C, real: u32) -> Vec<u8> {
    let mut honest = C::new("VkInstanceCreateInfo");
    honest.words.copy_from_slice(&ci.words);
    honest.set_u32("enabledExtensionCount", real);
    let wire = encode(&honest);
    let marker = real.to_le_bytes();
    let target = 0x0BAD_F00Du32.to_le_bytes();
    // Blok licznika to put_bytes: [len=4][4 bajty]; w strumieniu pierwsze wystąpienie wzorca [4, real,0,0,0] po polu `flags`.
    let pattern = [4u8, marker[0], marker[1], marker[2], marker[3]];
    let pos = wire.windows(5).rposition(|w| w == pattern).expect("blok licznika");
    let mut out = wire.clone();
    out[pos + 1..pos + 5].copy_from_slice(&target);
    out
}

#[test]
fn every_truncation_of_a_valid_stream_is_an_error() {
    let mut app = C::new("VkApplicationInfo");
    let name = std::ffi::CString::new("x").unwrap();
    app.set_ptr("pApplicationName", name.as_ptr());
    let mut ci = C::new("VkInstanceCreateInfo");
    ci.set_ptr("pApplicationInfo", app.ptr());
    let wire = encode(&ci);
    for cut in 0..wire.len() {
        assert!(decode_struct(ci.meta, &mut Decoder::new(&wire[..cut]), &Identity).is_err(), "cięcie {cut}/{}", wire.len());
    }
}

#[test]
fn random_bytes_never_panic_and_arena_stays_bounded() {
    let mut x: u64 = 0x1234_5678_9ABC_DEF1;
    let mut next = || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let names = ["VkInstanceCreateInfo", "VkDeviceCreateInfo", "VkShaderModuleCreateInfo", "VkPhysicalDeviceFeatures2", "VkBufferCreateInfo"];
    for i in 0..3000 {
        let meta = find_struct(names[i % names.len()]).unwrap();
        let len = (next() % 160) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        if let Ok(d) = decode_struct(meta, &mut Decoder::new(&bytes), &Identity) {
            assert!(d.arena.used() <= gpu_vkcodec::codec::MAX_ARENA);
        }
    }
}

#[test]
fn self_referencing_chain_is_a_limit_error_not_a_hang() {
    let mut a = C::new("VkPhysicalDeviceVulkan12Features");
    let aptr = a.ptr();
    a.set_ptr("pNext", aptr); // pętla
    let mut f2 = C::new("VkPhysicalDeviceFeatures2");
    f2.set_ptr("pNext", a.ptr());
    let mut e = Encoder::new();
    assert_eq!(unsafe { encode_struct(f2.meta, f2.ptr(), &mut e, &Identity) }, Err(CodecError::Limit("długość łańcucha pNext")));
    assert_eq!(MAX_CHAIN, 64);
}

#[test]
fn decoder_rejects_oversized_chain_and_unknown_stype() {
    let f2 = find_struct("VkPhysicalDeviceFeatures2").unwrap();
    let mut e = Encoder::new();
    // sType i inne wartości: surowe bloki; potem łańcuch o długości 65.
    let c = C::new("VkPhysicalDeviceFeatures2");
    let honest = encode(&c);
    let mut w = honest.clone();
    // Ostatni bajt uczciwego strumienia to licznik łańcucha (0); zamieniamy go na 65.
    *w.last_mut().unwrap() = 65;
    assert!(decode_struct(f2, &mut Decoder::new(&w), &Identity).is_err());
    let mut w2 = honest.clone();
    *w2.last_mut().unwrap() = 1;
    w2.extend(0x7FFF_0000u32.to_le_bytes());
    assert_eq!(decode_struct(f2, &mut Decoder::new(&w2), &Identity).err(), Some(CodecError::UnknownStype(0x7FFF_0000)));
    e.put_u8(0);
}

#[test]
fn coverage_report_of_members_the_codec_cannot_carry() {
    // Nie wymusza zera: tylko liczy, ile członków (w strukturach z sType) wymaga jeszcze obsługi.
    let mut unsupported = 0;
    let mut total = 0;
    for e in STYPE_TABLE {
        let m = find_struct(e.name).unwrap();
        for x in m.members() {
            total += 1;
            let bad = matches!(x.shape, gpu_proto::meta::Shape::Bitfield)
                || (x.shape == gpu_proto::meta::Shape::Pointer && x.ptr_depth == 2 && !(x.elem == gpu_proto::meta::Elem::Char))
                || (x.shape == gpu_proto::meta::Shape::Value && matches!(x.elem, gpu_proto::meta::Elem::Void | gpu_proto::meta::Elem::External));
            if bad {
                unsupported += 1;
            }
        }
    }
    eprintln!("członków nieobsługiwanych: {unsupported} z {total}");
    assert!(unsupported * 20 < total, "{unsupported}/{total}");
}

// ---------- polecenia ----------

use gpu_proto::meta::find_command;
use gpu_vkcodec::codec::{decode_command, encode_command};

fn slots(vals: &[u64]) -> Vec<u64> {
    vals.to_vec()
}

fn enc_cmd(name: &str, args: &[u64], ctx: &dyn Ctx) -> Vec<u8> {
    let (op, _) = find_command(name).unwrap();
    let mut e = Encoder::new();
    unsafe { encode_command(op, args.as_ptr() as *const u8, &mut e, ctx) }.unwrap();
    e.finish()
}

#[test]
fn fill_buffer_values_roundtrip_with_handle_translation() {
    struct Plus;
    impl Ctx for Plus {
        fn handle_to_wire(&self, _: &str, r: u64) -> Result<u64, CodecError> { Ok(r + 1000) }
        fn wire_to_handle(&self, _: &str, w: u64) -> Result<u64, CodecError> { Ok(w - 1000) }
    }
    // vkCmdFillBuffer(commandBuffer, dstBuffer, dstOffset, size, data)
    let args = slots(&[11, 22, 256, 4096, 0xDEAD_BEEF]);
    let wire = enc_cmd("vkCmdFillBuffer", &args, &Plus);
    let d = decode_command(&mut Decoder::new(&wire), &Plus).unwrap().unwrap();
    assert_eq!(d.command.name, "vkCmdFillBuffer");
    let got: Vec<u64> = (0..5).map(|i| unsafe { std::ptr::read_unaligned(d.args.ptr.add(i * 8) as *const u64) }).collect();
    assert_eq!(got[0], 11, "uchwyt wraca do wartości serwera");
    assert_eq!(got[1], 22);
    assert_eq!((got[2], got[3], got[4] & 0xFFFF_FFFF), (256, 4096, 0xDEAD_BEEF));
}

#[test]
fn create_buffer_keeps_the_client_assigned_output_id_untranslated() {
    struct Plus;
    impl Ctx for Plus {
        fn handle_to_wire(&self, _: &str, r: u64) -> Result<u64, CodecError> { Ok(r + 1000) }
        fn wire_to_handle(&self, _: &str, w: u64) -> Result<u64, CodecError> { Ok(w - 1000) }
    }
    let mut ci = C::new("VkBufferCreateInfo");
    ci.set_u64("size", 65536);
    ci.set_u32("usage", 0x20);
    let id_slot: u64 = 77; // *pBuffer: identyfikator nadany przez klienta
    let args = slots(&[5, ci.ptr() as u64, 0, &id_slot as *const u64 as u64]);
    let wire = enc_cmd("vkCreateBuffer", &args, &Plus);
    let d = decode_command(&mut Decoder::new(&wire), &Plus).unwrap().unwrap();
    unsafe {
        assert_eq!(std::ptr::read_unaligned(d.args.ptr as *const u64), 5, "device przetłumaczony");
        let info = std::ptr::read_unaligned(d.args.ptr.add(8) as *const *const u8);
        assert_eq!(std::ptr::read_unaligned(info.add(off("VkBufferCreateInfo", "size")) as *const u64), 65536);
        assert_eq!(std::ptr::read_unaligned(d.args.ptr.add(16) as *const u64), 0, "pAllocator zawsze null");
        let out = std::ptr::read_unaligned(d.args.ptr.add(24) as *const *const u64);
        assert_eq!(*out, 77, "uchwyt wyjściowy bez tłumaczenia");
    }
}

#[test]
fn allocate_command_buffers_count_comes_from_the_info_struct() {
    let mut ai = C::new("VkCommandBufferAllocateInfo");
    ai.set_u64("commandPool", 3);
    ai.set_u32("commandBufferCount", 3);
    let ids: [u64; 3] = [101, 102, 103];
    let args = slots(&[5, ai.ptr() as u64, ids.as_ptr() as u64]);
    let wire = enc_cmd("vkAllocateCommandBuffers", &args, &Identity);
    let d = decode_command(&mut Decoder::new(&wire), &Identity).unwrap().unwrap();
    unsafe {
        let out = std::ptr::read_unaligned(d.args.ptr.add(16) as *const *const u64);
        assert_eq!((*out, *out.add(1), *out.add(2)), (101, 102, 103));
    }
}

#[test]
fn blend_constants_array_and_submit_struct_array() {
    let bc = [0.25f32, 0.5, 0.75, 1.0];
    let wire = enc_cmd("vkCmdSetBlendConstants", &slots(&[9, bc.as_ptr() as u64]), &Identity);
    let d = decode_command(&mut Decoder::new(&wire), &Identity).unwrap().unwrap();
    unsafe {
        let p = std::ptr::read_unaligned(d.args.ptr.add(8) as *const *const f32);
        assert_eq!((*p, *p.add(1), *p.add(2), *p.add(3)), (0.25, 0.5, 0.75, 1.0));
    }
    let mut si = C::new("VkSubmitInfo");
    let cbs: [u64; 2] = [31, 32];
    si.set_u32("commandBufferCount", 2);
    si.set_ptr("pCommandBuffers", cbs.as_ptr());
    let wire = enc_cmd("vkQueueSubmit", &slots(&[4, 1, si.ptr() as u64, 8]), &Identity);
    let d = decode_command(&mut Decoder::new(&wire), &Identity).unwrap().unwrap();
    unsafe {
        let subs = std::ptr::read_unaligned(d.args.ptr.add(16) as *const *const u8);
        let cbp = std::ptr::read_unaligned(subs.add(off("VkSubmitInfo", "pCommandBuffers")) as *const *const u64);
        assert_eq!((*cbp, *cbp.add(1)), (31, 32));
        assert_eq!(std::ptr::read_unaligned(d.args.ptr.add(24) as *const u64), 8, "fence");
    }
}

#[test]
fn command_streams_decode_sequentially_and_reject_bad_opcodes() {
    let mut all = Vec::new();
    all.extend(enc_cmd("vkCmdFillBuffer", &slots(&[1, 2, 0, 16, 7]), &Identity));
    all.extend(enc_cmd("vkEndCommandBuffer", &slots(&[1]), &Identity));
    let mut dec = Decoder::new(&all);
    assert_eq!(decode_command(&mut dec, &Identity).unwrap().unwrap().command.name, "vkCmdFillBuffer");
    assert_eq!(decode_command(&mut dec, &Identity).unwrap().unwrap().command.name, "vkEndCommandBuffer");
    assert!(decode_command(&mut dec, &Identity).unwrap().is_none(), "czysty koniec");
    assert!(decode_command(&mut Decoder::new(&[0xFF, 0xFF]), &Identity).is_err(), "kod operacji poza tabelą");
    let (op, _) = find_command("vkGetPhysicalDeviceXcbPresentationSupportKHR").unwrap();
    let mut e = Encoder::new();
    assert!(unsafe { encode_command(op, [0u64; 4].as_ptr() as *const u8, &mut e, &Identity) }.is_err(), "nieobsługiwane polecenie");
}

#[test]
fn random_command_streams_never_panic() {
    let mut x: u64 = 0xABCD_EF01_2345_6789;
    let mut next = || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let ops: Vec<u16> = ["vkCreateBuffer", "vkQueueSubmit", "vkCreateGraphicsPipelines", "vkAllocateCommandBuffers", "vkCmdPipelineBarrier"].iter().map(|n| find_command(n).unwrap().0).collect();
    for i in 0..3000 {
        let mut bytes = ops[i % ops.len()].to_le_bytes().to_vec();
        let len = (next() % 200) as usize;
        bytes.extend((0..len).map(|_| next() as u8));
        let _ = decode_command(&mut Decoder::new(&bytes), &Identity);
    }
}
