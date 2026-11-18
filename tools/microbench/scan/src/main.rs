//! Micro-benchmark for 4 methods of detecting dirty 4 KiB pages in a large anonymous mmap region.
//! Methods:
//!   A "pmscan": userfaultfd async write-protect + PAGEMAP_SCAN (kernel 6.7+)
//!   B "softdirty": /proc/self/clear_refs + /proc/self/pagemap soft-dirty bits
//!   C "hash": per-page 64-bit hashes (CRC32C via SSE4.2 if available, else fallback)
//!   D "memcmp": full shadow copy + slice comparison

use std::arch::x86_64::_mm_crc32_u64;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::ptr;
use std::time::{Duration, Instant};

const PAGE: usize = 4096;
const PAGEMAP_ENTRY_SIZE: usize = 8;
const PM_SOFT_DIRTY: u64 = 1 << 55;

// --- Method A constants (from memtrack_ref.rs) ---
const SYS_USERFAULTFD: i64 = 323;
const O_CLOEXEC: i64 = 0o2000000;
const O_NONBLOCK: i64 = 0o4000;
const UFFD_USER_MODE_ONLY: i64 = 1;
const UFFD_API: u64 = 0xAA;
const UFFD_FEATURE_WP_UNPOPULATED: u64 = 1 << 13;
const UFFD_FEATURE_WP_ASYNC: u64 = 1 << 15;
const UFFDIO_API: u64 = 0xc018_aa3f;
const UFFDIO_REGISTER: u64 = 0xc020_aa00;
const UFFDIO_WRITEPROTECT: u64 = 0xc018_aa06;
const UFFDIO_REGISTER_MODE_WP: u64 = 2;
const UFFDIO_WRITEPROTECT_MODE_WP: u64 = 1;
const PAGEMAP_SCAN: u64 = 0xc060_6610;
const PM_SCAN_WP_MATCHING: u64 = 1;
const PM_SCAN_CHECK_WPASYNC: u64 = 2;
const PAGE_IS_WRITTEN: u64 = 2;

#[repr(C)]
struct UffdioApi {
    api: u64,
    features: u64,
    ioctls: u64,
}

#[repr(C)]
struct UffdioRegister {
    start: u64,
    len: u64,
    mode: u64,
    ioctls: u64,
}

#[repr(C)]
struct UffdioWriteProtect {
    start: u64,
    len: u64,
    mode: u64,
}

#[repr(C)]
struct PmScanArg {
    size: u64,
    flags: u64,
    start: u64,
    end: u64,
    walk_end: u64,
    vec: u64,
    vec_len: u64,
    max_pages: u64,
    category_inverted: u64,
    category_mask: u64,
    category_anyof_mask: u64,
    return_mask: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PageRegion {
    start: u64,
    end: u64,
    categories: u64,
}

/// mmap z opcjonalnym wyłączeniem stron transparentnych (SCAN_NOHUGE=1): przy THP=always śledzenie soft-dirty działa na stronach 2 MiB.
unsafe fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8 {
    let p = mmap_raw(addr, len, prot, flags, fd, offset);
    if p != libc::MAP_FAILED as *mut u8 && std::env::var_os("SCAN_NOHUGE").is_some() {
        madvise(p, len, 15); // MADV_NOHUGEPAGE
    }
    p
}

extern "C" {
    #[link_name = "mmap"]
    fn mmap_raw(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn syscall(num: i64, ...) -> i64;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn getrusage(who: i32, usage: *mut Rusage) -> i32;
}

#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i64,
}

#[repr(C)]
struct Rusage {
    ru_utime: Timeval,
    ru_stime: Timeval,
    ru_maxrss: i64,
    ru_ixrss: i64,
    ru_idrss: i64,
    ru_isrss: i64,
    ru_minflt: i64,
    ru_majflt: i64,
    ru_nswap: i64,
    ru_inblock: i64,
    ru_oublock: i64,
    ru_msgsnd: i64,
    ru_msgrcv: i64,
    ru_nsignals: i64,
    ru_nvcsw: i64,
    ru_nivcsw: i64,
}

const RUSAGE_SELF: i32 = 0;

fn get_cpu_time() -> Duration {
    unsafe {
        let mut usage = std::mem::zeroed::<Rusage>();
        getrusage(RUSAGE_SELF, &mut usage);
        Duration::new(
            usage.ru_utime.tv_sec as u64,
            (usage.ru_utime.tv_usec * 1000) as u32,
        ) + Duration::new(
            usage.ru_stime.tv_sec as u64,
            (usage.ru_stime.tv_usec * 1000) as u32,
        )
    }
}

// --- Xorshift64 PRNG ---
struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Xorshift64 { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_bounded(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

// --- CRC32C detection ---
#[inline]
fn has_sse42() -> bool {
    std::is_x86_feature_detected!("sse4.2")
}

#[inline]
fn crc32c_u64(crc: u32, data: u64) -> u32 {
    unsafe { _mm_crc32_u64(crc as u64, data) as u32 }
}

#[inline]
fn hash_page_crc32c(page: &[u8]) -> u64 {
    let mut crc: u32 = 0xFFFFFFFF;
    let chunks = page.chunks_exact(8);
    let rem = chunks.remainder();
    for chunk in chunks {
        let val = u64::from_le_bytes(chunk.try_into().unwrap());
        crc = crc32c_u64(crc, val);
    }
    if !rem.is_empty() {
        let mut buf = [0u8; 8];
        buf[..rem.len()].copy_from_slice(rem);
        crc = crc32c_u64(crc, u64::from_le_bytes(buf));
    }
    (!crc) as u64
}

#[inline]
fn hash_page_fallback(page: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let chunks = page.chunks_exact(8);
    let rem = chunks.remainder();
    for chunk in chunks {
        let val = u64::from_le_bytes(chunk.try_into().unwrap());
        hash ^= val;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    if !rem.is_empty() {
        let mut buf = [0u8; 8];
        buf[..rem.len()].copy_from_slice(rem);
        hash ^= u64::from_le_bytes(buf);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[inline]
fn hash_page(page: &[u8]) -> u64 {
    if has_sse42() {
        hash_page_crc32c(page)
    } else {
        hash_page_fallback(page)
    }
}

// --- Method A: userfaultfd + PAGEMAP_SCAN ---
struct MethodA {
    _uffd: Option<OwnedFd>,
    pagemap: File,
    base: usize,
    len: usize,
}

impl MethodA {
    fn new(ptr: *mut u8, len: usize) -> Result<Self, String> {
        let base = ptr as usize;
        if !base.is_multiple_of(PAGE) || len == 0 || !len.is_multiple_of(PAGE) {
            return Err("unaligned region".to_string());
        }
        unsafe {
            let fd = syscall(SYS_USERFAULTFD, O_CLOEXEC | O_NONBLOCK | UFFD_USER_MODE_ONLY);
            if fd < 0 {
                return Err(format!("userfaultfd syscall failed: errno {}", -*libc::__errno_location()));
            }
            let uffd = OwnedFd::from_raw_fd(fd as i32);
            let mut api = UffdioApi {
                api: UFFD_API,
                features: UFFD_FEATURE_WP_ASYNC | UFFD_FEATURE_WP_UNPOPULATED,
                ioctls: 0,
            };
            if ioctl(uffd.as_raw_fd(), UFFDIO_API, &mut api as *mut UffdioApi) < 0 {
                return Err(format!("UFFDIO_API failed: errno {}", -*libc::__errno_location()));
            }
            let mut reg = UffdioRegister {
                start: base as u64,
                len: len as u64,
                mode: UFFDIO_REGISTER_MODE_WP,
                ioctls: 0,
            };
            if ioctl(uffd.as_raw_fd(), UFFDIO_REGISTER, &mut reg as *mut UffdioRegister) < 0 {
                return Err(format!("UFFDIO_REGISTER failed: errno {}", -*libc::__errno_location()));
            }
            let mut wp = UffdioWriteProtect {
                start: base as u64,
                len: len as u64,
                mode: UFFDIO_WRITEPROTECT_MODE_WP,
            };
            if ioctl(uffd.as_raw_fd(), UFFDIO_WRITEPROTECT, &mut wp as *mut UffdioWriteProtect) < 0 {
                return Err(format!("UFFDIO_WRITEPROTECT failed: errno {}", -*libc::__errno_location()));
            }
            let pagemap = File::open("/proc/self/pagemap").map_err(|e| e.to_string())?;
            let t = MethodA {
                _uffd: Some(uffd),
                pagemap,
                base,
                len,
            };
            // Clear initial dirty state
            t.take_dirty(0, len)?;
            Ok(t)
        }
    }

    fn take_dirty(&self, offset: usize, len: usize) -> Result<Vec<(usize, usize)>, String> {
        if !offset.is_multiple_of(PAGE) || !len.is_multiple_of(PAGE) || offset.checked_add(len).ok_or("overflow")? > self.len {
            return Err("invalid range".to_string());
        }
        let mut out: Vec<(usize, usize)> = Vec::new();
        let (mut cur, end) = ((self.base + offset) as u64, (self.base + offset + len) as u64);
        let mut regions = [PageRegion::default(); 256];
        while cur < end {
            let mut arg = PmScanArg {
                size: std::mem::size_of::<PmScanArg>() as u64,
                flags: PM_SCAN_WP_MATCHING | PM_SCAN_CHECK_WPASYNC,
                start: cur,
                end,
                walk_end: 0,
                vec: regions.as_mut_ptr() as u64,
                vec_len: regions.len() as u64,
                max_pages: 0,
                category_inverted: 0,
                category_mask: PAGE_IS_WRITTEN,
                category_anyof_mask: 0,
                return_mask: PAGE_IS_WRITTEN,
            };
            let n = unsafe { ioctl(self.pagemap.as_raw_fd(), PAGEMAP_SCAN, &mut arg as *mut PmScanArg) };
            if n < 0 {
                return Err(format!("PAGEMAP_SCAN failed: errno {}", unsafe { -*libc::__errno_location() }));
            }
            for r in &regions[..n as usize] {
                let (a, b) = (r.start as usize - self.base, r.end as usize - self.base);
                match out.last_mut() {
                    Some(last) if last.1 == a => last.1 = b,
                    _ => out.push((a, b)),
                }
            }
            if arg.walk_end <= cur {
                break;
            }
            cur = arg.walk_end;
        }
        Ok(out)
    }
}

// --- Method B: soft-dirty ---
struct MethodB {
    pagemap: File,
    base: usize,
    len: usize,
    num_pages: usize,
}

impl MethodB {
    fn new(ptr: *mut u8, len: usize) -> Result<Self, String> {
        let base = ptr as usize;
        let num_pages = len / PAGE;
        let pagemap = File::open("/proc/self/pagemap").map_err(|e| e.to_string())?;
        // Clear soft-dirty bits
        let mut clear_refs = File::options()
            .write(true)
            .open("/proc/self/clear_refs")
            .map_err(|e| e.to_string())?;
        clear_refs.write_all(b"4").map_err(|e| e.to_string())?;
        Ok(MethodB {
            pagemap,
            base,
            len,
            num_pages,
        })
    }

    fn scan(&mut self) -> Result<Vec<usize>, String> {
        let mut buf = vec![0u8; self.num_pages * PAGEMAP_ENTRY_SIZE];
        self.pagemap
            .seek(SeekFrom::Start((self.base / PAGE * PAGEMAP_ENTRY_SIZE) as u64))
            .map_err(|e| e.to_string())?;
        self.pagemap.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let mut dirty = Vec::new();
        for (i, chunk) in buf.chunks_exact(PAGEMAP_ENTRY_SIZE).enumerate() {
            let entry = u64::from_le_bytes(chunk.try_into().unwrap());
            if entry & PM_SOFT_DIRTY != 0 {
                dirty.push(i * PAGE);
            }
        }
        // Clear soft-dirty bits for next scan
        let mut clear_refs = File::options()
            .write(true)
            .open("/proc/self/clear_refs")
            .map_err(|e| e.to_string())?;
        clear_refs.write_all(b"4").map_err(|e| e.to_string())?;
        Ok(dirty)
    }
}

// --- Method C: hash ---
struct MethodC {
    base: usize,
    len: usize,
    num_pages: usize,
    hashes: Vec<u64>,
    use_crc32c: bool,
}

impl MethodC {
    fn new(ptr: *mut u8, len: usize) -> Self {
        let base = ptr as usize;
        let num_pages = len / PAGE;
        let use_crc32c = has_sse42();
        let mut hashes = Vec::with_capacity(num_pages);
        unsafe {
            for i in 0..num_pages {
                let page_ptr = (base + i * PAGE) as *const u8;
                let page = std::slice::from_raw_parts(page_ptr, PAGE);
                hashes.push(hash_page(page));
            }
        }
        MethodC {
            base,
            len,
            num_pages,
            hashes,
            use_crc32c,
        }
    }

    fn scan(&mut self) -> Vec<usize> {
        let mut dirty = Vec::new();
        unsafe {
            for i in 0..self.num_pages {
                let page_ptr = (self.base + i * PAGE) as *const u8;
                let page = std::slice::from_raw_parts(page_ptr, PAGE);
                let h = hash_page(page);
                if h != self.hashes[i] {
                    self.hashes[i] = h;
                    dirty.push(i * PAGE);
                }
            }
        }
        dirty
    }
}

// --- Method D: memcmp ---
struct MethodD {
    base: usize,
    len: usize,
    num_pages: usize,
    shadow: Vec<u8>,
}

impl MethodD {
    fn new(ptr: *mut u8, len: usize) -> Self {
        let base = ptr as usize;
        let num_pages = len / PAGE;
        let mut shadow = vec![0u8; len];
        unsafe {
            ptr::copy_nonoverlapping(ptr, shadow.as_mut_ptr(), len);
        }
        MethodD {
            base,
            len,
            num_pages,
            shadow,
        }
    }

    fn scan(&mut self) -> Vec<usize> {
        let mut dirty = Vec::new();
        unsafe {
            for i in 0..self.num_pages {
                let page_ptr = (self.base + i * PAGE) as *const u8;
                let shadow_ptr = self.shadow.as_ptr().add(i * PAGE);
                let page = std::slice::from_raw_parts(page_ptr, PAGE);
                let shadow_page = std::slice::from_raw_parts(shadow_ptr, PAGE);
                if page != shadow_page {
                    ptr::copy_nonoverlapping(page_ptr, self.shadow.as_mut_ptr().add(i * PAGE), PAGE);
                    dirty.push(i * PAGE);
                }
            }
        }
        dirty
    }
}

// --- Statistics ---
struct Stats {
    values: Vec<f64>,
}

impl Stats {
    fn new() -> Self {
        Stats { values: Vec::new() }
    }

    fn push(&mut self, v: f64) {
        self.values.push(v);
    }

    fn mean(&self) -> f64 {
        self.values.iter().sum::<f64>() / self.values.len() as f64
    }

    fn percentile(&self, p: f64) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        let mut sorted = self.values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
        sorted[idx]
    }
}

// --- Main benchmark ---
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 || args.len() > 5 {
        eprintln!("Usage: {} <region_MB> <dirty_pages_per_iter> <iters> [seed]", args[0]);
        std::process::exit(1);
    }

    let region_mb: usize = args[1].parse().expect("Invalid region_MB");
    let dirty_pages: usize = args[2].parse().expect("Invalid dirty_pages_per_iter");
    let iters: usize = args[3].parse().expect("Invalid iters");
    let seed: u64 = if args.len() == 5 {
        args[4].parse().expect("Invalid seed")
    } else {
        0x123456789ABCDEF0
    };

    let region_size = region_mb * 1024 * 1024;
    let num_pages = region_size / PAGE;

    if dirty_pages > num_pages {
        eprintln!("dirty_pages_per_iter ({}) exceeds total pages ({})", dirty_pages, num_pages);
        std::process::exit(1);
    }

    println!("Region: {} MiB ({} pages), dirty pages/iter: {}, iters: {}, seed: 0x{:016x}",
             region_mb, num_pages, dirty_pages, iters, seed);
    println!("{:<10} | {:>12} {:>12} {:>12} | {:>12} | {:>12} | {:>12}",
             "method", "scan_mean", "scan_p50", "scan_p99", "writes", "total", "cpu/iter");
    println!("{}", "-".repeat(100));

    // Generate deterministic dirty page sequences for all methods
    let mut rng = Xorshift64::new(seed);
    let mut all_dirty_sets: Vec<Vec<usize>> = Vec::with_capacity(iters);
    for _ in 0..iters {
        let mut dirty_set = Vec::with_capacity(dirty_pages);
        let mut used = std::collections::HashSet::new();
        while dirty_set.len() < dirty_pages {
            let page_idx = rng.next_bounded(num_pages as u64) as usize;
            if used.insert(page_idx) {
                dirty_set.push(page_idx * PAGE);
            }
        }
        dirty_set.sort();
        all_dirty_sets.push(dirty_set);
    }

    // --- Method A ---
    {
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                region_size,
                3, // PROT_READ | PROT_WRITE
                0x22, // MAP_PRIVATE | MAP_ANONYMOUS
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED as *mut u8 {
            eprintln!("mmap failed for method A");
            std::process::exit(1);
        }
        // Pre-fault
        unsafe {
            for i in 0..num_pages {
                *ptr.add(i * PAGE) = 0xAB;
            }
        }

        match MethodA::new(ptr, region_size) {
            Ok(mut method) => {
                let mut scan_stats = Stats::new();
                let mut write_stats = Stats::new();
                let mut cpu_stats = Stats::new();
                let mut total_scan_ns = 0u128;
                let mut total_write_ns = 0u128;
                let mut total_cpu_ns = 0u128;

                for (iter, dirty_set) in all_dirty_sets.iter().enumerate() {
                    // Time writes
                    let write_start = Instant::now();
                    unsafe {
                        for &offset in dirty_set {
                            std::ptr::write_unaligned(ptr.add(offset) as *mut u64, (iter as u64 + 1) | 0x5A5A_0000_0000_0000);
                        }
                    }
                    let write_dur = write_start.elapsed();
                    write_stats.push(write_dur.as_nanos() as f64 / 1000.0);
                    total_write_ns += write_dur.as_nanos();

                    // Time scan
                    let cpu_before = get_cpu_time();
                    let scan_start = Instant::now();
                    let result = method.take_dirty(0, region_size).expect("Method A scan failed");
                    let scan_dur = scan_start.elapsed();
                    let cpu_after = get_cpu_time();
                    let cpu_dur = cpu_after - cpu_before;
                    scan_stats.push(scan_dur.as_nanos() as f64 / 1000.0);
                    cpu_stats.push(cpu_dur.as_nanos() as f64 / 1000.0);
                    total_scan_ns += scan_dur.as_nanos();
                    total_cpu_ns += cpu_dur.as_nanos();

                    // Verify
                    let mut reported = Vec::new();
                    for (start, end) in result {
                        for off in (start..end).step_by(PAGE) {
                            reported.push(off);
                        }
                    }
                    reported.sort();
                    if reported != *dirty_set {
                        panic!("Method A verification failed at iter {}: expected {:?}, got {:?}",
                               iter, dirty_set, reported);
                    }
                }

                println!("{:<10} | {:>12.2} {:>12.2} {:>12.2} | {:>12.2} | {:>12.2} | {:>12.2}",
                         "pmscan",
                         scan_stats.mean(),
                         scan_stats.percentile(50.0),
                         scan_stats.percentile(99.0),
                         write_stats.mean(),
                         (total_scan_ns + total_write_ns) as f64 / iters as f64 / 1000.0,
                         cpu_stats.mean());
            }
            Err(e) => {
                println!("{:<10} | UNSUPPORTED: {}", "pmscan", e);
            }
        }

        unsafe {
            munmap(ptr, region_size);
        }
    }

    // --- Method B ---
    {
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                region_size,
                3,
                0x22,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED as *mut u8 {
            eprintln!("mmap failed for method B");
            std::process::exit(1);
        }
        unsafe {
            for i in 0..num_pages {
                *ptr.add(i * PAGE) = 0xAB;
            }
        }

        match MethodB::new(ptr, region_size) {
            Ok(mut method) => {
                let mut scan_stats = Stats::new();
                let mut write_stats = Stats::new();
                let mut cpu_stats = Stats::new();
                let mut total_scan_ns = 0u128;
                let mut total_write_ns = 0u128;
                let mut total_cpu_ns = 0u128;

                for (iter, dirty_set) in all_dirty_sets.iter().enumerate() {
                    let write_start = Instant::now();
                    unsafe {
                        for &offset in dirty_set {
                            std::ptr::write_unaligned(ptr.add(offset) as *mut u64, (iter as u64 + 1) | 0x5A5A_0000_0000_0000);
                        }
                    }
                    let write_dur = write_start.elapsed();
                    write_stats.push(write_dur.as_nanos() as f64 / 1000.0);
                    total_write_ns += write_dur.as_nanos();

                    let cpu_before = get_cpu_time();
                    let scan_start = Instant::now();
                    let result = method.scan().expect("Method B scan failed");
                    let scan_dur = scan_start.elapsed();
                    let cpu_after = get_cpu_time();
                    let cpu_dur = cpu_after - cpu_before;
                    scan_stats.push(scan_dur.as_nanos() as f64 / 1000.0);
                    cpu_stats.push(cpu_dur.as_nanos() as f64 / 1000.0);
                    total_scan_ns += scan_dur.as_nanos();
                    total_cpu_ns += cpu_dur.as_nanos();

                    let mut reported = result;
                    reported.sort();
                    if reported != *dirty_set {
                        panic!("Method B verification failed at iter {}: expected {:?}, got {:?}",
                               iter, dirty_set, reported);
                    }
                }

                println!("{:<10} | {:>12.2} {:>12.2} {:>12.2} | {:>12.2} | {:>12.2} | {:>12.2}",
                         "softdirty",
                         scan_stats.mean(),
                         scan_stats.percentile(50.0),
                         scan_stats.percentile(99.0),
                         write_stats.mean(),
                         (total_scan_ns + total_write_ns) as f64 / iters as f64 / 1000.0,
                         cpu_stats.mean());
            }
            Err(e) => {
                println!("{:<10} | UNSUPPORTED: {}", "softdirty", e);
            }
        }

        unsafe {
            munmap(ptr, region_size);
        }
    }

    // --- Method C ---
    {
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                region_size,
                3,
                0x22,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED as *mut u8 {
            eprintln!("mmap failed for method C");
            std::process::exit(1);
        }
        unsafe {
            for i in 0..num_pages {
                *ptr.add(i * PAGE) = 0xAB;
            }
        }

        let mut method = MethodC::new(ptr, region_size);
        let mut scan_stats = Stats::new();
        let mut write_stats = Stats::new();
        let mut cpu_stats = Stats::new();
        let mut total_scan_ns = 0u128;
        let mut total_write_ns = 0u128;
        let mut total_cpu_ns = 0u128;

        for (iter, dirty_set) in all_dirty_sets.iter().enumerate() {
            let write_start = Instant::now();
            unsafe {
                for &offset in dirty_set {
                    std::ptr::write_unaligned(ptr.add(offset) as *mut u64, (iter as u64 + 1) | 0x5A5A_0000_0000_0000);
                }
            }
            let write_dur = write_start.elapsed();
            write_stats.push(write_dur.as_nanos() as f64 / 1000.0);
            total_write_ns += write_dur.as_nanos();

            let cpu_before = get_cpu_time();
            let scan_start = Instant::now();
            let result = method.scan();
            let scan_dur = scan_start.elapsed();
            let cpu_after = get_cpu_time();
            let cpu_dur = cpu_after - cpu_before;
            scan_stats.push(scan_dur.as_nanos() as f64 / 1000.0);
            cpu_stats.push(cpu_dur.as_nanos() as f64 / 1000.0);
            total_scan_ns += scan_dur.as_nanos();
            total_cpu_ns += cpu_dur.as_nanos();

            let mut reported = result;
            reported.sort();
            if reported != *dirty_set {
                panic!("Method C verification failed at iter {}: expected {:?}, got {:?}",
                       iter, dirty_set, reported);
            }
        }

        println!("{:<10} | {:>12.2} {:>12.2} {:>12.2} | {:>12.2} | {:>12.2} | {:>12.2}",
                 "hash",
                 scan_stats.mean(),
                 scan_stats.percentile(50.0),
                 scan_stats.percentile(99.0),
                 write_stats.mean(),
                 (total_scan_ns + total_write_ns) as f64 / iters as f64 / 1000.0,
                 cpu_stats.mean());

        unsafe {
            munmap(ptr, region_size);
        }
    }

    // --- Method D ---
    {
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                region_size,
                3,
                0x22,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED as *mut u8 {
            eprintln!("mmap failed for method D");
            std::process::exit(1);
        }
        unsafe {
            for i in 0..num_pages {
                *ptr.add(i * PAGE) = 0xAB;
            }
        }

        let mut method = MethodD::new(ptr, region_size);
        let mut scan_stats = Stats::new();
        let mut write_stats = Stats::new();
        let mut cpu_stats = Stats::new();
        let mut total_scan_ns = 0u128;
        let mut total_write_ns = 0u128;
        let mut total_cpu_ns = 0u128;

        for (iter, dirty_set) in all_dirty_sets.iter().enumerate() {
            let write_start = Instant::now();
            unsafe {
                for &offset in dirty_set {
                    std::ptr::write_unaligned(ptr.add(offset) as *mut u64, (iter as u64 + 1) | 0x5A5A_0000_0000_0000);
                }
            }
            let write_dur = write_start.elapsed();
            write_stats.push(write_dur.as_nanos() as f64 / 1000.0);
            total_write_ns += write_dur.as_nanos();

            let cpu_before = get_cpu_time();
            let scan_start = Instant::now();
            let result = method.scan();
            let scan_dur = scan_start.elapsed();
            let cpu_after = get_cpu_time();
            let cpu_dur = cpu_after - cpu_before;
            scan_stats.push(scan_dur.as_nanos() as f64 / 1000.0);
            cpu_stats.push(cpu_dur.as_nanos() as f64 / 1000.0);
            total_scan_ns += scan_dur.as_nanos();
            total_cpu_ns += cpu_dur.as_nanos();

            let mut reported = result;
            reported.sort();
            if reported != *dirty_set {
                panic!("Method D verification failed at iter {}: expected {:?}, got {:?}",
                       iter, dirty_set, reported);
            }
        }

        println!("{:<10} | {:>12.2} {:>12.2} {:>12.2} | {:>12.2} | {:>12.2} | {:>12.2}",
                 "memcmp",
                 scan_stats.mean(),
                 scan_stats.percentile(50.0),
                 scan_stats.percentile(99.0),
                 write_stats.mean(),
                 (total_scan_ns + total_write_ns) as f64 / iters as f64 / 1000.0,
                 cpu_stats.mean());

        unsafe {
            munmap(ptr, region_size);
        }
    }
}
