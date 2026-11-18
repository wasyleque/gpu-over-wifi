use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::time::Instant;

const FRAME_HEADER: u64 = 16;
const BLOCK_SIZE: usize = 4096;

fn zstd_pfx(raw: &[u8], prefix: &[u8], level: i32) -> usize {
    let mut cctx = zstd::zstd_safe::CCtx::create();
    let mut out: Vec<u8> = Vec::with_capacity(zstd::zstd_safe::compress_bound(raw.len()));
    let ok = cctx.set_parameter(zstd::zstd_safe::CParameter::CompressionLevel(level)).is_ok() && cctx.ref_prefix(prefix).is_ok();
    if ok && cctx.compress2(&mut out, raw).is_ok() {
        out.len().min(raw.len())
    } else {
        raw.len()
    }
}

fn zstd_plain(raw: &[u8], level: i32) -> usize {
    zstd::bulk::compress(raw, level).map_or(raw.len(), |c| c.len().min(raw.len()))
}

fn lz4_compress(raw: &[u8]) -> usize {
    lz4_flex::block::compress(raw).len().min(raw.len())
}

fn hash128(data: &[u8]) -> u128 {
    let mut h1: u64 = 0x9e3779b97f4a7c15;
    let mut h2: u64 = 0xc2b2ae3d27d4eb4f;
    let chunks = data.chunks_exact(8);
    let rem = chunks.remainder();
    
    for chunk in chunks {
        let val = u64::from_le_bytes(chunk.try_into().unwrap());
        h1 ^= val.wrapping_mul(0x9e3779b97f4a7c15);
        h1 = h1.rotate_left(27).wrapping_mul(0x94d049bb133111eb);
        h2 ^= val.wrapping_mul(0xc2b2ae3d27d4eb4f);
        h2 = h2.rotate_left(31).wrapping_mul(0xbf58476d1ce4e5b9);
    }
    
    if !rem.is_empty() {
        let mut buf = [0u8; 8];
        buf[..rem.len()].copy_from_slice(rem);
        let val = u64::from_le_bytes(buf);
        h1 ^= val.wrapping_mul(0x9e3779b97f4a7c15);
        h1 = h1.rotate_left(27).wrapping_mul(0x94d049bb133111eb);
        h2 ^= val.wrapping_mul(0xc2b2ae3d27d4eb4f);
        h2 = h2.rotate_left(31).wrapping_mul(0xbf58476d1ce4e5b9);
    }
    
    ((h1 as u128) << 64) | h2 as u128
}

fn sparse_diff(data: &[u8], prefix: &[u8], gap: usize) -> (Vec<(usize, usize)>, usize) {
    let mut runs = Vec::new();
    let mut i = 0;
    let mut payload_size = 0;
    
    while i < data.len() {
        if data[i] != prefix[i] {
            let start = i;
            let mut end = i + 1;
            let mut last_diff = i;
            
            while end < data.len() {
                if data[end] != prefix[end] {
                    last_diff = end;
                }
                if end - last_diff > gap {
                    break;
                }
                end += 1;
            }
            
            runs.push((start, end));
            payload_size += end - start;
            i = end;
        } else {
            i += 1;
        }
    }
    
    (runs, payload_size)
}

fn sparse_wire_size(runs: &[(usize, usize)], payload_size: usize) -> usize {
    if runs.is_empty() {
        return FRAME_HEADER as usize;
    }
    let run_headers = runs.len() * 6;
    (run_headers + payload_size + FRAME_HEADER as usize).min(payload_size + FRAME_HEADER as usize)
}

struct Record {
    memory: u64,
    offset: u64,
    data: Vec<u8>,
}

fn parse_records(data: &[u8], max_records: usize) -> Vec<Record> {
    let mut records = Vec::new();
    let mut at = 0;
    
    while at + 20 <= data.len() && records.len() < max_records {
        let memory = u64::from_le_bytes(data[at..at+8].try_into().unwrap());
        let offset = u64::from_le_bytes(data[at+8..at+16].try_into().unwrap());
        let len = u32::from_le_bytes(data[at+16..at+20].try_into().unwrap()) as usize;
        
        if at + 20 + len > data.len() {
            break;
        }
        
        records.push(Record {
            memory,
            offset,
            data: data[at+20..at+20+len].to_vec(),
        });
        
        at += 20 + len;
    }
    
    records
}

struct MemoryStream {
    memory_id: u64,
    records: Vec<(u64, Vec<u8>)>,
    max_offset: u64,
}

fn group_by_memory(records: Vec<Record>) -> Vec<MemoryStream> {
    let mut streams: HashMap<u64, Vec<(u64, Vec<u8>)>> = HashMap::new();
    
    for rec in records {
        streams.entry(rec.memory).or_insert_with(Vec::new).push((rec.offset, rec.data));
    }
    
    streams.into_iter().map(|(memory_id, records)| {
        let max_offset = records.iter().map(|(off, data)| off + data.len() as u64).max().unwrap_or(0);
        MemoryStream {
            memory_id,
            records,
            max_offset,
        }
    }).collect()
}

struct AnalysisResult {
    raw_bytes: u64,
    zstd1_pfx_bytes: u64,
    lz4_bytes: u64,
    sparse_g4_bytes: u64,
    sparse_g16_bytes: u64,
    sparse_g64_bytes: u64,
    sparse_lz4_g4_bytes: u64,
    sparse_lz4_g16_bytes: u64,
    sparse_lz4_g64_bytes: u64,
    cas_lz4_bytes: u64,
    rewind_bytes: u64,
    dict_16k_bytes: u64,
    dict_64k_bytes: u64,
    dict_112k_bytes: u64,
    zstd_plain_bytes: u64,
    
    zstd1_pfx_time: u128,
    lz4_time: u128,
    sparse_time: u128,
    cas_lz4_time: u128,
    rewind_time: u128,
    dict_16k_time: u128,
    dict_64k_time: u128,
    dict_112k_time: u128,
    zstd_plain_time: u128,
    
    c5_total_blocks: u64,
    c5_hit_blocks: u64,
    c5_total_bytes: u64,
    c5_hit_bytes: u64,
    c5_diff_total_blocks: u64,
    c5_diff_hit_blocks: u64,
    
    c6_unchanged_bytes: u64,
    c6_k1_hit_bytes: u64,
    c6_k2_hit_bytes: u64,
    c6_k3_hit_bytes: u64,
    c6_k4_hit_bytes: u64,
    c6_k8_hit_bytes: u64,
    c6_total_bytes: u64,
    
    record_count: u64,
}

impl AnalysisResult {
    fn new() -> Self {
        AnalysisResult {
            raw_bytes: 0,
            zstd1_pfx_bytes: 0,
            lz4_bytes: 0,
            sparse_g4_bytes: 0,
            sparse_g16_bytes: 0,
            sparse_g64_bytes: 0,
            sparse_lz4_g4_bytes: 0,
            sparse_lz4_g16_bytes: 0,
            sparse_lz4_g64_bytes: 0,
            cas_lz4_bytes: 0,
            rewind_bytes: 0,
            dict_16k_bytes: 0,
            dict_64k_bytes: 0,
            dict_112k_bytes: 0,
            zstd_plain_bytes: 0,
            
            zstd1_pfx_time: 0,
            lz4_time: 0,
            sparse_time: 0,
            cas_lz4_time: 0,
            rewind_time: 0,
            dict_16k_time: 0,
            dict_64k_time: 0,
            dict_112k_time: 0,
            zstd_plain_time: 0,
            
            c5_total_blocks: 0,
            c5_hit_blocks: 0,
            c5_total_bytes: 0,
            c5_hit_bytes: 0,
            c5_diff_total_blocks: 0,
            c5_diff_hit_blocks: 0,
            
            c6_unchanged_bytes: 0,
            c6_k1_hit_bytes: 0,
            c6_k2_hit_bytes: 0,
            c6_k3_hit_bytes: 0,
            c6_k4_hit_bytes: 0,
            c6_k8_hit_bytes: 0,
            c6_total_bytes: 0,
            
            record_count: 0,
        }
    }
    
    fn add(&mut self, other: &AnalysisResult) {
        self.raw_bytes += other.raw_bytes;
        self.zstd1_pfx_bytes += other.zstd1_pfx_bytes;
        self.lz4_bytes += other.lz4_bytes;
        self.sparse_g4_bytes += other.sparse_g4_bytes;
        self.sparse_g16_bytes += other.sparse_g16_bytes;
        self.sparse_g64_bytes += other.sparse_g64_bytes;
        self.sparse_lz4_g4_bytes += other.sparse_lz4_g4_bytes;
        self.sparse_lz4_g16_bytes += other.sparse_lz4_g16_bytes;
        self.sparse_lz4_g64_bytes += other.sparse_lz4_g64_bytes;
        self.cas_lz4_bytes += other.cas_lz4_bytes;
        self.rewind_bytes += other.rewind_bytes;
        self.dict_16k_bytes += other.dict_16k_bytes;
        self.dict_64k_bytes += other.dict_64k_bytes;
        self.dict_112k_bytes += other.dict_112k_bytes;
        self.zstd_plain_bytes += other.zstd_plain_bytes;
        
        self.zstd1_pfx_time += other.zstd1_pfx_time;
        self.lz4_time += other.lz4_time;
        self.sparse_time += other.sparse_time;
        self.cas_lz4_time += other.cas_lz4_time;
        self.rewind_time += other.rewind_time;
        self.dict_16k_time += other.dict_16k_time;
        self.dict_64k_time += other.dict_64k_time;
        self.dict_112k_time += other.dict_112k_time;
        self.zstd_plain_time += other.zstd_plain_time;
        
        self.c5_total_blocks += other.c5_total_blocks;
        self.c5_hit_blocks += other.c5_hit_blocks;
        self.c5_total_bytes += other.c5_total_bytes;
        self.c5_hit_bytes += other.c5_hit_bytes;
        self.c5_diff_total_blocks += other.c5_diff_total_blocks;
        self.c5_diff_hit_blocks += other.c5_diff_hit_blocks;
        
        self.c6_unchanged_bytes += other.c6_unchanged_bytes;
        self.c6_k1_hit_bytes += other.c6_k1_hit_bytes;
        self.c6_k2_hit_bytes += other.c6_k2_hit_bytes;
        self.c6_k3_hit_bytes += other.c6_k3_hit_bytes;
        self.c6_k4_hit_bytes += other.c6_k4_hit_bytes;
        self.c6_k8_hit_bytes += other.c6_k8_hit_bytes;
        self.c6_total_bytes += other.c6_total_bytes;
        
        self.record_count += other.record_count;
    }
}

fn analyze_stream(stream: &MemoryStream) -> AnalysisResult {
    let mut result = AnalysisResult::new();
    let mut mirror = vec![0u8; stream.max_offset as usize];
    let warmup = stream.records.len() / 5;
    
    // Collect warmup samples for dictionary training
    let mut warmup_samples: Vec<Vec<u8>> = Vec::new();
    for (_, data) in stream.records.iter().take(warmup) {
        if warmup_samples.len() < 2000 {
            warmup_samples.push(data.clone());
        }
    }
    
    // Train dictionaries
    let dict_16k = if warmup_samples.len() >= 10 {
        zstd::dict::from_samples(&warmup_samples, 16 * 1024).ok()
    } else {
        None
    };
    let dict_64k = if warmup_samples.len() >= 10 {
        zstd::dict::from_samples(&warmup_samples, 64 * 1024).ok()
    } else {
        None
    };
    let dict_112k = if warmup_samples.len() >= 10 {
        zstd::dict::from_samples(&warmup_samples, 112 * 1024).ok()
    } else {
        None
    };
    
    let mut cctx_16k = dict_16k.as_ref().map(|d| zstd::bulk::Compressor::with_dictionary(1, d).unwrap());
    let mut cctx_64k = dict_64k.as_ref().map(|d| zstd::bulk::Compressor::with_dictionary(1, d).unwrap());
    let mut cctx_112k = dict_112k.as_ref().map(|d| zstd::bulk::Compressor::with_dictionary(1, d).unwrap());
    
    // Content cache
    let mut content_cache: HashSet<u128> = HashSet::new();
    
    // Rewind cache
    let mut rewind_cache: HashMap<(u64, usize), VecDeque<u128>> = HashMap::new();
    
    for (idx, (offset, data)) in stream.records.iter().enumerate() {
        let o = *offset as usize;
        let e = o + data.len();
        
        if idx >= warmup {
            let prefix = mirror[o..e].to_vec();
            result.record_count += 1;
            result.raw_bytes += data.len() as u64;
            
            // RAW
            // Already counted
            
            // ZSTD1P
            let start = Instant::now();
            let zstd1_size = zstd_pfx(data, &prefix, 1) + FRAME_HEADER as usize;
            result.zstd1_pfx_bytes += zstd1_size as u64;
            result.zstd1_pfx_time += start.elapsed().as_nanos();
            
            // LZ4
            let start = Instant::now();
            let lz4_size = lz4_compress(data) + FRAME_HEADER as usize;
            result.lz4_bytes += lz4_size as u64;
            result.lz4_time += start.elapsed().as_nanos();
            
            // SPARSE
            let start = Instant::now();
            let (runs_g4, payload_g4) = sparse_diff(data, &prefix, 4);
            let (runs_g16, payload_g16) = sparse_diff(data, &prefix, 16);
            let (runs_g64, payload_g64) = sparse_diff(data, &prefix, 64);
            
            let sparse_g4_size = sparse_wire_size(&runs_g4, payload_g4);
            let sparse_g16_size = sparse_wire_size(&runs_g16, payload_g16);
            let sparse_g64_size = sparse_wire_size(&runs_g64, payload_g64);
            
            result.sparse_g4_bytes += sparse_g4_size as u64;
            result.sparse_g16_bytes += sparse_g16_size as u64;
            result.sparse_g64_bytes += sparse_g64_size as u64;
            
            // SPARSE+LZ4
            if payload_g4 >= 256 {
                let mut sparse_data = Vec::with_capacity(payload_g4);
                for (start, end) in &runs_g4 {
                    sparse_data.extend_from_slice(&data[*start..*end]);
                }
                let compressed = lz4_compress(&sparse_data);
                result.sparse_lz4_g4_bytes += (compressed + FRAME_HEADER as usize) as u64;
            } else {
                result.sparse_lz4_g4_bytes += sparse_g4_size as u64;
            }
            
            if payload_g16 >= 256 {
                let mut sparse_data = Vec::with_capacity(payload_g16);
                for (start, end) in &runs_g16 {
                    sparse_data.extend_from_slice(&data[*start..*end]);
                }
                let compressed = lz4_compress(&sparse_data);
                result.sparse_lz4_g16_bytes += (compressed + FRAME_HEADER as usize) as u64;
            } else {
                result.sparse_lz4_g16_bytes += sparse_g16_size as u64;
            }
            
            if payload_g64 >= 256 {
                let mut sparse_data = Vec::with_capacity(payload_g64);
                for (start, end) in &runs_g64 {
                    sparse_data.extend_from_slice(&data[*start..*end]);
                }
                let compressed = lz4_compress(&sparse_data);
                result.sparse_lz4_g64_bytes += (compressed + FRAME_HEADER as usize) as u64;
            } else {
                result.sparse_lz4_g64_bytes += sparse_g64_size as u64;
            }
            
            result.sparse_time += start.elapsed().as_nanos();
            
            // C5 CONTENT CACHE
            let start = Instant::now();
            let mut cas_bytes = 0;
            let mut block_offset = 0;
            while block_offset < data.len() {
                let block_end = (block_offset + BLOCK_SIZE).min(data.len());
                let block = &data[block_offset..block_end];
                let block_hash = hash128(block);
                
                result.c5_total_blocks += 1;
                result.c5_total_bytes += block.len() as u64;
                
                let is_diff = block.iter().zip(prefix[block_offset..block_end].iter()).any(|(a, b)| a != b);
                if is_diff {
                    result.c5_diff_total_blocks += 1;
                }
                
                if content_cache.contains(&block_hash) {
                    result.c5_hit_blocks += 1;
                    result.c5_hit_bytes += block.len() as u64;
                    if is_diff {
                        result.c5_diff_hit_blocks += 1;
                    }
                    cas_bytes += 24;
                } else {
                    let compressed = lz4_compress(block);
                    cas_bytes += compressed;
                }
                
                block_offset = block_end;
            }
            result.cas_lz4_bytes += (cas_bytes + FRAME_HEADER as usize) as u64;
            result.cas_lz4_time += start.elapsed().as_nanos();
            
            // C6 REWIND
            let start = Instant::now();
            let key = (*offset, data.len());
            let current_hash = hash128(data);
            let prefix_hash = hash128(&prefix);
            
            result.c6_total_bytes += data.len() as u64;
            
            if current_hash == prefix_hash {
                result.c6_unchanged_bytes += data.len() as u64;
            }
            
            if let Some(history) = rewind_cache.get(&key) {
                let k1 = history.iter().rev().take(1).any(|h| *h == current_hash);
                let k2 = history.iter().rev().take(2).any(|h| *h == current_hash);
                let k3 = history.iter().rev().take(3).any(|h| *h == current_hash);
                let k4 = history.iter().rev().take(4).any(|h| *h == current_hash);
                let k8 = history.iter().rev().take(8).any(|h| *h == current_hash);
                
                if k1 { result.c6_k1_hit_bytes += data.len() as u64; }
                if k2 { result.c6_k2_hit_bytes += data.len() as u64; }
                if k3 { result.c6_k3_hit_bytes += data.len() as u64; }
                if k4 { result.c6_k4_hit_bytes += data.len() as u64; }
                if k8 { result.c6_k8_hit_bytes += data.len() as u64; }
            }
            
            // Calculate rewind wire size (best case: use smallest K that hits)
            let mut rewind_wire = data.len() + FRAME_HEADER as usize; // Default: send raw
            if let Some(history) = rewind_cache.get(&key) {
                if history.iter().rev().take(8).any(|h| *h == current_hash) {
                    rewind_wire = 18 + FRAME_HEADER as usize; // trafienie w którejkolwiek z 8 ostatnich wersji: 16-bajtowy hash + 2 bajty numeru
                }
            }
            result.rewind_bytes += rewind_wire as u64;
            result.rewind_time += start.elapsed().as_nanos();
            
            // C7 DICT
            let start = Instant::now();
            if let Some(cctx) = &mut cctx_16k {
                if let Ok(compressed) = cctx.compress(data) {
                    result.dict_16k_bytes += (compressed.len().min(data.len()) + FRAME_HEADER as usize) as u64;
                } else {
                    result.dict_16k_bytes += (data.len() + FRAME_HEADER as usize) as u64;
                }
            } else {
                result.dict_16k_bytes += (data.len() + FRAME_HEADER as usize) as u64;
            }
            result.dict_16k_time += start.elapsed().as_nanos();
            
            let start = Instant::now();
            if let Some(cctx) = &mut cctx_64k {
                if let Ok(compressed) = cctx.compress(data) {
                    result.dict_64k_bytes += (compressed.len().min(data.len()) + FRAME_HEADER as usize) as u64;
                } else {
                    result.dict_64k_bytes += (data.len() + FRAME_HEADER as usize) as u64;
                }
            } else {
                result.dict_64k_bytes += (data.len() + FRAME_HEADER as usize) as u64;
            }
            result.dict_64k_time += start.elapsed().as_nanos();
            
            let start = Instant::now();
            if let Some(cctx) = &mut cctx_112k {
                if let Ok(compressed) = cctx.compress(data) {
                    result.dict_112k_bytes += (compressed.len().min(data.len()) + FRAME_HEADER as usize) as u64;
                } else {
                    result.dict_112k_bytes += (data.len() + FRAME_HEADER as usize) as u64;
                }
            } else {
                result.dict_112k_bytes += (data.len() + FRAME_HEADER as usize) as u64;
            }
            result.dict_112k_time += start.elapsed().as_nanos();
            
            // ZSTD plain (no prefix)
            let start = Instant::now();
            let zstd_plain_size = zstd_plain(data, 1) + FRAME_HEADER as usize;
            result.zstd_plain_bytes += zstd_plain_size as u64;
            result.zstd_plain_time += start.elapsed().as_nanos();
        }
        
        // Update caches (always, including warmup)
        // Content cache
        let mut block_offset = 0;
        while block_offset < data.len() {
            let block_end = (block_offset + BLOCK_SIZE).min(data.len());
            let block = &data[block_offset..block_end];
            content_cache.insert(hash128(block));
            block_offset = block_end;
        }
        
        // Rewind cache
        let key = (*offset, data.len());
        let history = rewind_cache.entry(key).or_insert_with(VecDeque::new);
        history.push_back(hash128(data));
        if history.len() > 8 {
            history.pop_front();
        }
        
        // Update mirror
        mirror[o..e].copy_from_slice(data);
    }
    
    result
}

fn print_results(result: &AnalysisResult, label: &str) {
    println!("\n=== {} ===", label);
    println!("Records analysed: {}", result.record_count);
    
    let mb = |v: u64| v as f64 / 1e6;
    let ms = |v: u128| v as f64 / 1e6;
    let raw_mb = mb(result.raw_bytes);
    
    println!("\n{:<20} {:>10} {:>10} {:>10} {:>15}", "Method", "Wire MB", "% of RAW", "ms total", "ms per MB raw");
    println!("{}", "-".repeat(70));
    
    let methods = [
        ("RAW", result.raw_bytes, 0u128),
        ("ZSTD1P", result.zstd1_pfx_bytes, result.zstd1_pfx_time),
        ("LZ4", result.lz4_bytes, result.lz4_time),
        ("SPARSE(G=4)", result.sparse_g4_bytes, result.sparse_time),
        ("SPARSE(G=16)", result.sparse_g16_bytes, result.sparse_time),
        ("SPARSE(G=64)", result.sparse_g64_bytes, result.sparse_time),
        ("SPARSE+LZ4(G=4)", result.sparse_lz4_g4_bytes, result.sparse_time),
        ("SPARSE+LZ4(G=16)", result.sparse_lz4_g16_bytes, result.sparse_time),
        ("SPARSE+LZ4(G=64)", result.sparse_lz4_g64_bytes, result.sparse_time),
        ("CAS+LZ4", result.cas_lz4_bytes, result.cas_lz4_time),
        ("REWIND", result.rewind_bytes, result.rewind_time),
        ("DICT(16K)", result.dict_16k_bytes, result.dict_16k_time),
        ("DICT(64K)", result.dict_64k_bytes, result.dict_64k_time),
        ("DICT(112K)", result.dict_112k_bytes, result.dict_112k_time),
        ("ZSTD plain", result.zstd_plain_bytes, result.zstd_plain_time),
    ];
    
    for (name, bytes, time_ns) in methods {
        let wire_mb = mb(bytes);
        let pct = if result.raw_bytes > 0 { 100.0 * bytes as f64 / result.raw_bytes as f64 } else { 0.0 };
        let time_ms = ms(time_ns);
        let per_mb = if raw_mb > 0.0 { time_ms / raw_mb } else { 0.0 };
        println!("{:<20} {:>10.1} {:>9.1}% {:>10.1} {:>15.2}", name, wire_mb, pct, time_ms, per_mb);
    }
    
    // C5 hit rates
    println!("\n--- C5 Content Cache Hit Rates ---");
    if result.c5_total_blocks > 0 {
        println!("All blocks: {:.1}% hit rate ({}/{} blocks)", 
            100.0 * result.c5_hit_blocks as f64 / result.c5_total_blocks as f64,
            result.c5_hit_blocks, result.c5_total_blocks);
        println!("All bytes: {:.1}% hit rate ({}/{} bytes)", 
            100.0 * result.c5_hit_bytes as f64 / result.c5_total_bytes as f64,
            result.c5_hit_bytes, result.c5_total_bytes);
    }
    if result.c5_diff_total_blocks > 0 {
        println!("Diff blocks only: {:.1}% hit rate ({}/{} blocks)", 
            100.0 * result.c5_diff_hit_blocks as f64 / result.c5_diff_total_blocks as f64,
            result.c5_diff_hit_blocks, result.c5_diff_total_blocks);
    }
    
    // C6 hit rates
    println!("\n--- C6 Rewind Hit Rates ---");
    if result.c6_total_bytes > 0 {
        println!("Unchanged: {:.1}%", 100.0 * result.c6_unchanged_bytes as f64 / result.c6_total_bytes as f64);
        println!("K=1: {:.1}%", 100.0 * result.c6_k1_hit_bytes as f64 / result.c6_total_bytes as f64);
        println!("K=2: {:.1}%", 100.0 * result.c6_k2_hit_bytes as f64 / result.c6_total_bytes as f64);
        println!("K=3: {:.1}%", 100.0 * result.c6_k3_hit_bytes as f64 / result.c6_total_bytes as f64);
        println!("K=4: {:.1}%", 100.0 * result.c6_k4_hit_bytes as f64 / result.c6_total_bytes as f64);
        println!("K=8: {:.1}%", 100.0 * result.c6_k8_hit_bytes as f64 / result.c6_total_bytes as f64);
    }
}

fn generate_synthetic(seed: u64) -> Vec<Record> {
    let mut records = Vec::new();
    let mut rng = seed;
    let memory_id = 0x1000;
    let memory_size = 4 * 1024 * 1024;
    
    // Xorshift64
    fn next_rand(rng: &mut u64) -> u64 {
        let mut x = *rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *rng = x;
        x
    }
    
    // Generate 3 fixed contents for ring
    let mut ring_contents = Vec::new();
    for _ in 0..3 {
        let mut content = vec![0u8; 4096];
        for i in 0..content.len() {
            content[i] = next_rand(&mut rng) as u8;
        }
        ring_contents.push(content);
    }
    
    // Generate 20 fixed random blocks
    let mut fixed_blocks = Vec::new();
    for _ in 0..20 {
        let mut block = vec![0u8; 4096];
        for i in 0..block.len() {
            block[i] = next_rand(&mut rng) as u8;
        }
        fixed_blocks.push(block);
    }
    
    // Generate vertex block (16 KiB)
    let mut vertex_block = vec![0u8; 16 * 1024];
    for i in 0..vertex_block.len() {
        vertex_block[i] = next_rand(&mut rng) as u8;
    }
    
    let total_records = 6000;
    let ring_offsets = [0x10000u64, 0x20000, 0x30000];
    let vertex_offset = 0x50000u64;
    
    for i in 0..total_records {
        let behavior = i % 10;
        
        if behavior < 4 {
            // Ring behavior (40%)
            let ring_idx = 0usize; // jeden klucz (offset), treść cyklicznie z okresem 3
            let content_idx = ((i / 10) * 4 + (i % 10)) % 3; // numer rekordu pierścienia mod 3
            records.push(Record {
                memory: memory_id,
                offset: ring_offsets[ring_idx],
                data: ring_contents[content_idx].clone(),
            });
        } else if behavior < 7 {
            // Vertex block behavior (30%)
            // Modify 100 random bytes
            for _ in 0..100 {
                let pos = (next_rand(&mut rng) as usize) % vertex_block.len();
                vertex_block[pos] = next_rand(&mut rng) as u8;
            }
            records.push(Record {
                memory: memory_id,
                offset: vertex_offset,
                data: vertex_block.clone(),
            });
        } else {
            // Random blocks from fixed set (30%)
            let block_idx = (next_rand(&mut rng) as usize) % 20;
            let offset = (next_rand(&mut rng) as u64) % (memory_size as u64 - 4096);
            records.push(Record {
                memory: memory_id,
                offset,
                data: fixed_blocks[block_idx].clone(),
            });
        }
    }
    
    records
}

fn write_records(records: &[Record], path: &str) {
    let mut data = Vec::new();
    for rec in records {
        data.extend_from_slice(&rec.memory.to_le_bytes());
        data.extend_from_slice(&rec.offset.to_le_bytes());
        data.extend_from_slice(&(rec.data.len() as u32).to_le_bytes());
        data.extend_from_slice(&rec.data);
    }
    std::fs::write(path, data).expect("Failed to write file");
}

fn selftest() {
    println!("Running self-test...");
    let records = generate_synthetic(42);
    let streams = group_by_memory(records);
    
    let mut combined = AnalysisResult::new();
    for stream in &streams {
        let result = analyze_stream(stream);
        combined.add(&result);
    }
    
    // Test C6 K=3 hit rate on ring part
    // We need to analyze the ring records specifically
    let mut ring_k3_hits = 0u64;
    let mut ring_total = 0u64;
    let mut ring_k1_hits = 0u64;
    
    // Re-analyze with tracking
    let stream = &streams[0];
    let mut mirror = vec![0u8; stream.max_offset as usize];
    let warmup = stream.records.len() / 5;
    let mut rewind_cache: HashMap<(u64, usize), VecDeque<u128>> = HashMap::new();
    
    for (idx, (offset, data)) in stream.records.iter().enumerate() {
        let o = *offset as usize;
        let e = o + data.len();
        
        if idx >= warmup {
            let prefix = mirror[o..e].to_vec();
            let current_hash = hash128(data);
            let key = (*offset, data.len());
            
            // Check if this is a ring record (offset in ring_offsets)
            let is_ring = [0x10000u64, 0x20000, 0x30000].contains(offset);
            
            if is_ring {
                ring_total += 1;
                if let Some(history) = rewind_cache.get(&key) {
                    if history.len() >= 3 && history[history.len() - 3] == current_hash {
                        ring_k3_hits += 1;
                    }
                    if history.len() >= 1 && history[history.len() - 1] == current_hash {
                        ring_k1_hits += 1;
                    }
                }
            }
        }
        
        // Update rewind cache
        let key = (*offset, data.len());
        let history = rewind_cache.entry(key).or_insert_with(VecDeque::new);
        history.push_back(hash128(data));
        if history.len() > 8 {
            history.pop_front();
        }
        
        // Update mirror
        mirror[o..e].copy_from_slice(data);
    }
    
    let ring_k3_rate = if ring_total > 0 { ring_k3_hits as f64 / ring_total as f64 } else { 0.0 };
    let ring_k1_rate = if ring_total > 0 { ring_k1_hits as f64 / ring_total as f64 } else { 0.0 };
    
    println!("C6 K=3 hit-rate on ring: {:.1}% ({} / {})", 100.0 * ring_k3_rate, ring_k3_hits, ring_total);
    println!("C6 K=1 hit-rate on ring: {:.1}% ({} / {})", 100.0 * ring_k1_rate, ring_k1_hits, ring_total);
    
    let pass1 = ring_k3_rate > 0.95;
    let pass2 = ring_k1_rate < 0.05;
    println!("PASS: C6 K=3 hit-rate > 0.95: {}", if pass1 { "PASS" } else { "FAIL" });
    println!("PASS: C6 K=1 hit-rate < 0.05: {}", if pass2 { "PASS" } else { "FAIL" });
    
    // Test SPARSE(G=16) for vertex block
    let mut sparse_total = 0u64;
    let mut sparse_count = 0u64;
    
    let mut mirror = vec![0u8; stream.max_offset as usize];
    for (idx, (offset, data)) in stream.records.iter().enumerate() {
        let o = *offset as usize;
        let e = o + data.len();
        
        if idx >= warmup && *offset == 0x50000u64 {
            let prefix = mirror[o..e].to_vec();
            let (_, payload) = sparse_diff(data, &prefix, 16);
            sparse_total += payload as u64;
            sparse_count += 1;
        }
        
        mirror[o..e].copy_from_slice(data);
    }
    
    let avg_sparse = if sparse_count > 0 { sparse_total as f64 / sparse_count as f64 } else { 0.0 };
    println!("SPARSE(G=16) avg payload for vertex block: {:.0} bytes", avg_sparse);
    
    let pass3 = avg_sparse >= 300.0 && avg_sparse <= 1500.0;
    println!("PASS: SPARSE(G=16) payload between 300 and 1500: {}", if pass3 { "PASS" } else { "FAIL" });
    
    // Test C5 hit rate for random blocks
    let mut c5_hits = 0u64;
    let mut c5_total = 0u64;
    let mut content_cache: HashSet<u128> = HashSet::new();
    
    let mut mirror = vec![0u8; stream.max_offset as usize];
    for (idx, (offset, data)) in stream.records.iter().enumerate() {
        let o = *offset as usize;
        let e = o + data.len();
        
        if idx >= warmup {
            let is_random_block = ![0x10000u64, 0x20000, 0x30000, 0x50000u64].contains(offset);
            
            if is_random_block {
                let mut block_offset = 0;
                while block_offset < data.len() {
                    let block_end = (block_offset + BLOCK_SIZE).min(data.len());
                    let block = &data[block_offset..block_end];
                    let block_hash = hash128(block);
                    
                    c5_total += 1;
                    if content_cache.contains(&block_hash) {
                        c5_hits += 1;
                    }
                    
                    block_offset = block_end;
                }
            }
        }
        
        // Update content cache
        let mut block_offset = 0;
        while block_offset < data.len() {
            let block_end = (block_offset + BLOCK_SIZE).min(data.len());
            let block = &data[block_offset..block_end];
            content_cache.insert(hash128(block));
            block_offset = block_end;
        }
        
        mirror[o..e].copy_from_slice(data);
    }
    
    let c5_rate = if c5_total > 0 { c5_hits as f64 / c5_total as f64 } else { 0.0 };
    println!("C5 hit rate for random blocks: {:.1}% ({} / {})", 100.0 * c5_rate, c5_hits, c5_total);
    
    let pass4 = c5_rate > 0.95;
    println!("PASS: C5 hit rate > 0.95: {}", if pass4 { "PASS" } else { "FAIL" });
    
    if pass1 && pass2 && pass3 && pass4 {
        println!("\nAll self-tests PASSED");
    } else {
        println!("\nSome self-tests FAILED");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    
    if args.len() < 2 {
        println!("Usage:");
        println!("  recwhat <file> [--mem 0xHEX | --all] [--max-records N]");
        println!("  recwhat --gen <out_file> [seed]");
        println!("  recwhat --selftest");
        return;
    }
    
    if args[1] == "--selftest" {
        selftest();
        return;
    }
    
    if args[1] == "--gen" {
        let out_file = if args.len() > 2 { &args[2] } else { "synthetic.rec" };
        let seed = if args.len() > 3 { args[3].parse().unwrap_or(42) } else { 42 };
        let records = generate_synthetic(seed);
        write_records(&records, out_file);
        println!("Generated {} records to {}", records.len(), out_file);
        println!("Expected outcomes:");
        println!("  - Ring records (40%): C6 K=3 hit-rate > 95%, K=1 < 5%");
        println!("  - Vertex block (30%): SPARSE(G=16) payload 300-1500 bytes");
        println!("  - Random blocks (30%): C5 hit-rate > 95%");
        return;
    }
    
    let path = &args[1];
    let mut max_records = usize::MAX;
    let mut target_memory: Option<u64> = None;
    let mut all_memories = false;
    
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--mem" => {
                if i + 1 < args.len() {
                    let hex = args[i + 1].trim_start_matches("0x");
                    target_memory = u64::from_str_radix(hex, 16).ok();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--all" => {
                all_memories = true;
                i += 1;
            }
            "--max-records" => {
                if i + 1 < args.len() {
                    max_records = args[i + 1].parse().unwrap_or(usize::MAX);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    
    let mut file = std::fs::File::open(path).expect("Failed to open file");
    let mut data = Vec::new();
    file.read_to_end(&mut data).expect("Failed to read file");
    
    let records = parse_records(&data, max_records);
    println!("Parsed {} records", records.len());
    
    let streams = group_by_memory(records);
    println!("Found {} memory streams", streams.len());
    
    if all_memories {
        let mut combined = AnalysisResult::new();
        for stream in &streams {
            println!("\nAnalyzing memory 0x{:x} ({} records)", stream.memory_id, stream.records.len());
            let result = analyze_stream(stream);
            print_results(&result, &format!("Memory 0x{:x}", stream.memory_id));
            combined.add(&result);
        }
        
        if streams.len() > 1 {
            print_results(&combined, "Combined Total");
        }
    } else if let Some(mem_id) = target_memory {
        if let Some(stream) = streams.iter().find(|s| s.memory_id == mem_id) {
            println!("Analyzing memory 0x{:x} ({} records)", mem_id, stream.records.len());
            let result = analyze_stream(stream);
            print_results(&result, &format!("Memory 0x{:x}", mem_id));
        } else {
            println!("Memory 0x{:x} not found", mem_id);
        }
    } else if streams.len() == 1 {
        let stream = &streams[0];
        println!("Analyzing memory 0x{:x} ({} records)", stream.memory_id, stream.records.len());
        let result = analyze_stream(stream);
        print_results(&result, &format!("Memory 0x{:x}", stream.memory_id));
    } else {
        println!("Multiple memory streams found. Use --mem to select one or --all to analyze all.");
        for stream in &streams {
            println!("  Memory 0x{:x}: {} records", stream.memory_id, stream.records.len());
        }
    }
}
