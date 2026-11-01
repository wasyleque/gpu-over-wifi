//! Koder (klient, z pamięci aplikacji) i dekoder (serwer, do areny) struktur Vulkana.
//!
//! Format struktury: członkowie w kolejności z metadanych. Wartości surowe (skalary, napisy, unie) to bloki
//! `put_bytes`; uchwyty to `u64` po tłumaczeniu; wskaźnik to flaga (0 = null) i dane; `pNext` to liczba
//! struktur i kolejno `sType` + ciało (bez własnego `pNext`). Dekoder dwuprzebiegowy: najpierw wartości,
//! potem wskaźniki, a liczba elementów musi się zgadzać z członkiem wskazanym w `len`.

use gpu_proto::meta::{find_struct, Elem, MemberMeta, Shape, StructMeta};
use gpu_proto::stype::lookup;
use gpu_wire::{Decoder, Encoder};

use crate::len::eval_len;
use crate::CodecError;

pub const MAX_CHAIN: usize = 64;
pub const MAX_DEPTH: usize = 16;
pub const MAX_ARENA: usize = 64 * 1024 * 1024;
pub const MAX_ELEMS: u64 = 1 << 22;
const MAX_STRING: usize = 1 << 20;
const PNEXT_OFFSET: usize = 8;

/// Tłumaczenie uchwytów i polityka łańcucha `pNext`.
pub trait Ctx {
    /// Uchwyt klienta (wskaźnik lub identyfikator ICD) na wartość w protokole.
    fn handle_to_wire(&self, ty: &str, raw: u64) -> Result<u64, CodecError>;
    /// Wartość z protokołu na uchwyt serwera.
    fn wire_to_handle(&self, ty: &str, wire: u64) -> Result<u64, CodecError>;
    /// Czy struktura o tym `sType` może występować w łańcuchu (klient pomija nieobsługiwane, serwer je odrzuca).
    fn allow_stype(&self, _stype: u32) -> bool {
        true
    }
}

pub struct Identity;

impl Ctx for Identity {
    fn handle_to_wire(&self, _ty: &str, raw: u64) -> Result<u64, CodecError> {
        Ok(raw)
    }
    fn wire_to_handle(&self, _ty: &str, wire: u64) -> Result<u64, CodecError> {
        Ok(wire)
    }
}

fn meta_of(name: &str) -> Result<&'static StructMeta, CodecError> {
    find_struct(name).ok_or_else(|| CodecError::UnknownStruct(name.to_string()))
}

/// Wartość członka całkowitego (1, 2, 4 lub 8 B) z pamięci struktury, do wyrażeń długości.
unsafe fn read_uint(meta: &StructMeta, base: *const u8, name: &str) -> Option<u64> {
    // `wskaźnik->pole`: pierwszy człon to wskaźnik na strukturę, dalej szukamy w niej.
    if let Some((head, rest)) = name.split_once("->") {
        let m = meta.members().iter().find(|m| m.name == head && m.shape == Shape::Pointer && m.ptr_depth == 1 && m.elem == Elem::Struct)?;
        let p = std::ptr::read_unaligned(base.add(m.offset as usize) as *const *const u8);
        if p.is_null() {
            return None;
        }
        return read_uint(find_struct(m.type_name)?, p, rest);
    }
    let m = meta.members().iter().find(|m| m.name == name && m.shape == Shape::Value && m.elem == Elem::Scalar && m.count == 1)?;
    let p = base.add(m.offset as usize);
    Some(match m.elem_size {
        1 => *p as u64,
        2 => std::ptr::read_unaligned(p as *const u16) as u64,
        4 => std::ptr::read_unaligned(p as *const u32) as u64,
        8 => std::ptr::read_unaligned(p as *const u64),
        _ => return None,
    })
}

fn unsupported(meta: &StructMeta, m: &MemberMeta, why: &str) -> CodecError {
    CodecError::Unsupported(format!("{}.{}: {why}", meta.name, m.name))
}

// ======================= koder =======================

/// Koduje strukturę spod `ptr` (układ C x86_64 zgodny z `meta`).
///
/// # Safety
/// `ptr` musi wskazywać na poprawną, zainicjowaną strukturę opisaną przez `meta`, a wszystkie jej wskaźniki
/// (łącznie z łańcuchem `pNext`) na żywą pamięć aplikacji o rozmiarze wynikającym z pól długości.
pub unsafe fn encode_struct(meta: &StructMeta, ptr: *const u8, enc: &mut Encoder, ctx: &dyn Ctx) -> Result<(), CodecError> {
    enc_struct(meta, ptr, enc, ctx, 0, false)
}

unsafe fn enc_struct(meta: &StructMeta, ptr: *const u8, enc: &mut Encoder, ctx: &dyn Ctx, depth: usize, in_chain: bool) -> Result<(), CodecError> {
    if depth > MAX_DEPTH {
        return Err(CodecError::Limit("głębokość"));
    }
    if meta.is_union {
        if meta.members().iter().any(|m| m.shape != Shape::Value) {
            return Err(CodecError::Unsupported(format!("unia {} ze wskaźnikiem", meta.name)));
        }
        enc.put_bytes(std::slice::from_raw_parts(ptr, meta.size as usize));
        return Ok(());
    }
    // Przebieg 1: wartości; przebieg 2: wskaźniki i pNext (ta sama kolejność co w dekoderze).
    for m in meta.members() {
        match m.shape {
            Shape::Bitfield => return Err(unsupported(meta, m, "pole bitowe")),
            Shape::Value => enc_value(meta, m, ptr.add(m.offset as usize), enc, ctx, depth)?,
            Shape::Pointer | Shape::PNext => {}
        }
    }
    for m in meta.members() {
        match m.shape {
            Shape::PNext if !in_chain => enc_chain(std::ptr::read_unaligned(ptr.add(m.offset as usize) as *const *const u8), enc, ctx, depth)?,
            Shape::Pointer => enc_ptr(meta, m, ptr, enc, ctx, depth)?,
            _ => {}
        }
    }
    Ok(())
}

unsafe fn enc_value(meta: &StructMeta, m: &MemberMeta, at: *const u8, enc: &mut Encoder, ctx: &dyn Ctx, depth: usize) -> Result<(), CodecError> {
    let n = m.count as usize;
    match m.elem {
        Elem::Scalar | Elem::Char | Elem::Union => {
            if m.elem == Elem::Union {
                let u = meta_of(m.type_name)?;
                if u.members().iter().any(|x| x.shape != Shape::Value) {
                    return Err(unsupported(meta, m, "unia ze wskaźnikiem"));
                }
            }
            enc.put_bytes(std::slice::from_raw_parts(at, m.elem_size as usize * n));
        }
        Elem::Handle => {
            for i in 0..n {
                let raw = std::ptr::read_unaligned(at.add(i * 8) as *const u64);
                enc.put_u64(ctx.handle_to_wire(m.type_name, raw)?);
            }
        }
        Elem::Struct => {
            let sm = meta_of(m.type_name)?;
            for i in 0..n {
                enc_struct(sm, at.add(i * m.elem_size as usize), enc, ctx, depth + 1, false)?;
            }
        }
        Elem::FuncPtr => {}
        Elem::Void | Elem::External => return Err(unsupported(meta, m, "typ wartości nieprzekazywalny")),
    }
    Ok(())
}

unsafe fn cstr_bytes<'a>(p: *const u8) -> Result<&'a [u8], CodecError> {
    let mut n = 0;
    while n <= MAX_STRING {
        if *p.add(n) == 0 {
            return Ok(std::slice::from_raw_parts(p, n));
        }
        n += 1;
    }
    Err(CodecError::Limit("napis"))
}

unsafe fn enc_ptr(meta: &StructMeta, m: &MemberMeta, base: *const u8, enc: &mut Encoder, ctx: &dyn Ctx, depth: usize) -> Result<(), CodecError> {
    let p = std::ptr::read_unaligned(base.add(m.offset as usize) as *const *const u8);
    // Wskaźniki lokalne (typy zewnętrzne, funkcje, `void*` bez długości) nie przechodzą przez sieć.
    let local = matches!(m.elem, Elem::External | Elem::FuncPtr) || (m.elem == Elem::Void && m.len.is_empty());
    if p.is_null() || local {
        enc.put_u8(0);
        return Ok(());
    }
    let get = |name: &str| read_uint(meta, base, name);
    let spec = if m.len.is_empty() { None } else { Some(eval_len(m.len, &get)?) };
    let count = spec.as_ref().and_then(|s| s.count).unwrap_or(1);
    if count > MAX_ELEMS {
        return Err(CodecError::Limit("liczba elementów"));
    }
    let nt = spec.as_ref().is_some_and(|s| s.null_terminated);
    enc.put_u8(1);
    match (m.ptr_depth, m.elem) {
        (1, Elem::Char) if nt => enc.put_bytes(cstr_bytes(p)?),
        (1, Elem::Scalar | Elem::Char | Elem::Void) => {
            enc.put_varint(count);
            let size = if m.elem == Elem::Void { 1 } else { m.elem_size as u64 };
            enc.put_bytes(std::slice::from_raw_parts(p, (count * size) as usize));
        }
        (1, Elem::Handle) => {
            enc.put_varint(count);
            for i in 0..count as usize {
                let raw = std::ptr::read_unaligned(p.add(i * 8) as *const u64);
                // Wyjściowe uchwyty (parametr nie-const) to identyfikatory nadane przez klienta: bez tłumaczenia.
                enc.put_u64(if m.is_const { ctx.handle_to_wire(m.type_name, raw)? } else { raw });
            }
        }
        (1, Elem::Struct) => {
            let sm = meta_of(m.type_name)?;
            enc.put_varint(count);
            for i in 0..count as usize {
                enc_struct(sm, p.add(i * m.elem_size as usize), enc, ctx, depth + 1, false)?;
            }
        }
        (2, Elem::Char) if nt => {
            enc.put_varint(count);
            for i in 0..count as usize {
                let s = std::ptr::read_unaligned(p.add(i * 8) as *const *const u8);
                if s.is_null() {
                    return Err(unsupported(meta, m, "pusty napis w tablicy"));
                }
                enc.put_bytes(cstr_bytes(s)?);
            }
        }
        _ => return Err(unsupported(meta, m, "rodzaj wskaźnika")),
    }
    Ok(())
}

unsafe fn enc_chain(mut p: *const u8, enc: &mut Encoder, ctx: &dyn Ctx, depth: usize) -> Result<(), CodecError> {
    let mut nodes: Vec<(u32, &'static StructMeta, *const u8)> = Vec::new();
    let mut steps = 0;
    while !p.is_null() {
        steps += 1;
        if steps > MAX_CHAIN {
            return Err(CodecError::Limit("długość łańcucha pNext"));
        }
        let st = std::ptr::read_unaligned(p as *const u32);
        if let Some(e) = lookup(st) {
            if ctx.allow_stype(st) {
                nodes.push((st, meta_of(e.name)?, p));
            }
        }
        p = std::ptr::read_unaligned(p.add(PNEXT_OFFSET) as *const *const u8);
    }
    enc.put_varint(nodes.len() as u64);
    for (st, sm, node) in nodes {
        enc.put_u32(st);
        enc_struct(sm, node, enc, ctx, depth + 1, true)?;
    }
    Ok(())
}

/// Koduje polecenie: kod operacji (`u16`) i parametry z `args` (8 B na parametr, w kolejności z `vk.xml`).
///
/// # Safety
/// `args` musi wskazywać na `8 * liczba_parametrów` bajtów z poprawnymi wartościami parametrów.
pub unsafe fn encode_command(opcode: u16, args: *const u8, enc: &mut Encoder, ctx: &dyn Ctx) -> Result<(), CodecError> {
    let cmd = gpu_proto::meta::COMMANDS.get(opcode as usize).ok_or(CodecError::Limit("kod operacji"))?;
    if !cmd.supported {
        return Err(CodecError::Unsupported(format!("polecenie {}", cmd.name)));
    }
    enc.put_u16(opcode);
    encode_struct(&cmd.as_struct(), args, enc, ctx)
}

// ======================= dekoder =======================

/// Pamięć, w której dekoder odtwarza struktury; adresy bloków są stabilne do zwolnienia areny.
#[derive(Default)]
pub struct Arena {
    blocks: Vec<Box<[u64]>>,
    used: usize,
}

impl Arena {
    fn alloc(&mut self, bytes: usize) -> Result<*mut u8, CodecError> {
        let words = bytes.div_ceil(8).max(1);
        self.used = self.used.checked_add(words * 8).filter(|u| *u <= MAX_ARENA).ok_or(CodecError::Limit("arena"))?;
        let mut b = vec![0u64; words].into_boxed_slice();
        let p = b.as_mut_ptr() as *mut u8;
        self.blocks.push(b);
        Ok(p)
    }

    pub fn used(&self) -> usize {
        self.used
    }
}

/// Odtworzona struktura: `ptr` wskazuje do wnętrza `arena` i jest ważny, dopóki żyje ten obiekt.
pub struct Decoded {
    pub arena: Arena,
    pub ptr: *mut u8,
}

pub fn decode_struct(meta: &StructMeta, dec: &mut Decoder, ctx: &dyn Ctx) -> Result<Decoded, CodecError> {
    let mut arena = Arena::default();
    let ptr = arena.alloc(meta.size as usize)?;
    // SAFETY: `ptr` wskazuje na świeży, wyzerowany blok o rozmiarze `meta.size`; wszystkie zapisy idą przez metadane.
    unsafe { dec_struct(meta, dec, ptr, &mut arena, ctx, 0, false)? };
    Ok(Decoded { arena, ptr })
}

unsafe fn put_ptr(dst: *mut u8, offset: u32, addr: *mut u8) {
    std::ptr::write_unaligned(dst.add(offset as usize) as *mut usize, addr as usize);
}

unsafe fn dec_struct(meta: &StructMeta, dec: &mut Decoder, dst: *mut u8, arena: &mut Arena, ctx: &dyn Ctx, depth: usize, in_chain: bool) -> Result<(), CodecError> {
    if depth > MAX_DEPTH {
        return Err(CodecError::Limit("głębokość"));
    }
    if meta.is_union {
        let b = dec.get_bytes()?;
        if b.len() != meta.size as usize {
            return Err(CodecError::Limit("rozmiar unii"));
        }
        std::ptr::copy_nonoverlapping(b.as_ptr(), dst, b.len());
        return Ok(());
    }
    // Przebieg 1: wartości (daje liczniki dla wyrażeń długości).
    for m in meta.members() {
        match m.shape {
            Shape::Bitfield => return Err(unsupported(meta, m, "pole bitowe")),
            Shape::Value => dec_value(meta, m, dec, dst.add(m.offset as usize), arena, ctx, depth)?,
            Shape::Pointer | Shape::PNext => {}
        }
    }
    // Przebieg 2: wskaźniki i pNext, w kolejności z kodera.
    for m in meta.members() {
        match m.shape {
            Shape::PNext if !in_chain => dec_chain(dec, dst.add(m.offset as usize), arena, ctx, depth)?,
            Shape::Pointer => dec_ptr(meta, m, dec, dst, arena, ctx, depth)?,
            _ => {}
        }
    }
    Ok(())
}

unsafe fn dec_value(meta: &StructMeta, m: &MemberMeta, dec: &mut Decoder, at: *mut u8, arena: &mut Arena, ctx: &dyn Ctx, depth: usize) -> Result<(), CodecError> {
    let n = m.count as usize;
    match m.elem {
        Elem::Scalar | Elem::Char | Elem::Union => {
            let b = dec.get_bytes()?;
            if b.len() != m.elem_size as usize * n {
                return Err(CodecError::Limit("rozmiar wartości"));
            }
            std::ptr::copy_nonoverlapping(b.as_ptr(), at, b.len());
        }
        Elem::Handle => {
            for i in 0..n {
                let w = dec.get_u64()?;
                std::ptr::write_unaligned(at.add(i * 8) as *mut u64, ctx.wire_to_handle(m.type_name, w)?);
            }
        }
        Elem::Struct => {
            let sm = meta_of(m.type_name)?;
            for i in 0..n {
                dec_struct(sm, dec, at.add(i * m.elem_size as usize), arena, ctx, depth + 1, false)?;
            }
        }
        Elem::FuncPtr => {}
        Elem::Void | Elem::External => return Err(unsupported(meta, m, "typ wartości nieprzekazywalny")),
    }
    Ok(())
}

fn check_count(meta: &StructMeta, m: &MemberMeta, dst: *const u8, n: u64) -> Result<(), CodecError> {
    if n > MAX_ELEMS {
        return Err(CodecError::Limit("liczba elementów"));
    }
    let expected = if m.len.is_empty() {
        Some(1)
    } else {
        // SAFETY: `dst` wskazuje na strukturę `meta` z już odtworzonymi wartościami (przebieg 1).
        let get = |name: &str| unsafe { read_uint(meta, dst, name) };
        eval_len(m.len, &get)?.count
    };
    match expected {
        Some(e) if e != n => Err(CodecError::Limit("liczba elementów niezgodna z polem długości")),
        _ => Ok(()),
    }
}

unsafe fn dec_ptr(meta: &StructMeta, m: &MemberMeta, dec: &mut Decoder, dst: *mut u8, arena: &mut Arena, ctx: &dyn Ctx, depth: usize) -> Result<(), CodecError> {
    if dec.get_u8()? == 0 {
        return Ok(());
    }
    let nt = !m.len.is_empty() && m.len.ends_with("null-terminated");
    match (m.ptr_depth, m.elem) {
        (1, Elem::Char) if nt && !m.len.contains(',') => {
            let s = dec.get_bytes()?;
            let p = arena.alloc(s.len() + 1)?;
            std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            put_ptr(dst, m.offset, p);
        }
        (1, Elem::Scalar | Elem::Char | Elem::Void) => {
            let n = dec.get_varint()?;
            check_count(meta, m, dst, n)?;
            let size = if m.elem == Elem::Void { 1 } else { m.elem_size as u64 };
            let b = dec.get_bytes()?;
            if b.len() as u64 != n * size {
                return Err(CodecError::Limit("rozmiar danych"));
            }
            let p = arena.alloc(b.len())?;
            std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len());
            put_ptr(dst, m.offset, p);
        }
        (1, Elem::Handle) => {
            let n = dec.get_varint()?;
            check_count(meta, m, dst, n)?;
            if n * 8 > dec.remaining() as u64 {
                return Err(CodecError::Limit("liczba elementów ponad dane"));
            }
            let p = arena.alloc(n as usize * 8)?;
            for i in 0..n as usize {
                let w = dec.get_u64()?;
                let v = if m.is_const { ctx.wire_to_handle(m.type_name, w)? } else { w };
                std::ptr::write_unaligned(p.add(i * 8) as *mut u64, v);
            }
            put_ptr(dst, m.offset, p);
        }
        (1, Elem::Struct) => {
            let n = dec.get_varint()?;
            check_count(meta, m, dst, n)?;
            if n > dec.remaining() as u64 {
                return Err(CodecError::Limit("liczba elementów ponad dane"));
            }
            let sm = meta_of(m.type_name)?;
            let p = arena.alloc(n as usize * m.elem_size as usize)?;
            for i in 0..n as usize {
                dec_struct(sm, dec, p.add(i * m.elem_size as usize), arena, ctx, depth + 1, false)?;
            }
            put_ptr(dst, m.offset, p);
        }
        (2, Elem::Char) if nt => {
            let n = dec.get_varint()?;
            check_count(meta, m, dst, n)?;
            if n > dec.remaining() as u64 {
                return Err(CodecError::Limit("liczba elementów ponad dane"));
            }
            let arr = arena.alloc(n as usize * 8)?;
            for i in 0..n as usize {
                let s = dec.get_bytes()?;
                let p = arena.alloc(s.len() + 1)?;
                std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
                put_ptr(arr, (i * 8) as u32, p);
            }
            put_ptr(dst, m.offset, arr);
        }
        _ => return Err(unsupported(meta, m, "rodzaj wskaźnika")),
    }
    Ok(())
}

unsafe fn dec_chain(dec: &mut Decoder, slot: *mut u8, arena: &mut Arena, ctx: &dyn Ctx, depth: usize) -> Result<(), CodecError> {
    let n = dec.get_varint()?;
    if n > MAX_CHAIN as u64 {
        return Err(CodecError::Limit("długość łańcucha pNext"));
    }
    let mut slot = slot;
    for _ in 0..n {
        let st = dec.get_u32()?;
        let e = lookup(st).ok_or(CodecError::UnknownStype(st))?;
        if !ctx.allow_stype(st) {
            return Err(CodecError::UnknownStype(st));
        }
        let sm = meta_of(e.name)?;
        let node = arena.alloc(sm.size as usize)?;
        dec_struct(sm, dec, node, arena, ctx, depth + 1, true)?;
        // Nie ufamy `sType` z ciała: węzeł ma typ, pod którym go przyjęliśmy.
        std::ptr::write_unaligned(node as *mut u32, st);
        std::ptr::write_unaligned(slot as *mut usize, node as usize);
        slot = node.add(PNEXT_OFFSET);
    }
    Ok(())
}

/// Odtworzone polecenie: `args` to 8-bajtowe sloty parametrów w arenie.
pub struct DecodedCommand {
    pub opcode: u16,
    pub command: &'static gpu_proto::meta::CommandMeta,
    pub args: Decoded,
}

/// Odczytuje kolejne polecenie ze strumienia. `Ok(None)` na czystym końcu strumienia.
pub fn decode_command(dec: &mut Decoder, ctx: &dyn Ctx) -> Result<Option<DecodedCommand>, CodecError> {
    if dec.remaining() == 0 {
        return Ok(None);
    }
    let opcode = dec.get_u16()?;
    let command = gpu_proto::meta::COMMANDS.get(opcode as usize).ok_or(CodecError::Limit("kod operacji"))?;
    if !command.supported {
        return Err(CodecError::Unsupported(format!("polecenie {}", command.name)));
    }
    let args = decode_struct(&command.as_struct(), dec, ctx)?;
    Ok(Some(DecodedCommand { opcode, command, args }))
}
