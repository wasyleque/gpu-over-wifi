//! Parsery plików `/proc` (CPU, pamięć, dyski, sieć) i obliczanie przyrostów między próbkami.
//! Dane z jądra mogą być niepełne lub uszkodzone: złe linie są pomijane, bez paniki.
#![allow(dead_code)]

/// Czasy jednego rdzenia CPU (`id = None` dla linii zbiorczej `cpu`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub id: Option<usize>,
    /// user + nice + system + irq + softirq + steal
    pub busy: u64,
    /// busy + idle + iowait
    pub total: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub total_kb: u64,
    pub available_kb: u64,
    pub cached_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskCounters {
    pub name: String,
    pub sectors_read: u64,
    pub sectors_written: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DiskRate {
    pub name: String,
    pub read_mb_s: f64,
    pub write_mb_s: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetCounters {
    pub iface: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NetRate {
    pub iface: String,
    pub rx_mbit_s: f64,
    pub tx_mbit_s: f64,
}

/// Linie `cpu` i `cpuN` z `/proc/stat` (zbiorcza pierwsza, jeśli jest w pliku).
pub fn parse_cpu_stat(text: &str) -> Vec<CpuTimes> {
    let mut result = Vec::new();
    
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        
        let first = parts[0];
        let id = if first == "cpu" {
            None
        } else if let Some(num_str) = first.strip_prefix("cpu") {
            if let Ok(num) = num_str.parse::<usize>() {
                Some(num)
            } else {
                continue; // Skip lines that don't match cpu<number>
            }
        } else {
            continue; // Skip lines that don't start with "cpu"
        };
        
        // Parse the numeric fields (user, nice, system, idle, iowait, irq, softirq, steal)
        let mut fields = [0u64; 8];
        let mut i = 0;
        for part in parts.iter().skip(1) {
            if i >= 8 {
                break;
            }
            if let Ok(val) = part.parse::<u64>() {
                fields[i] = val;
            } else {
                // If any field fails to parse, skip this line
                continue;
            }
            i += 1;
        }
        
        // Calculate busy and total times
        let busy = fields[0].saturating_add(fields[1])
            .saturating_add(fields[2])
            .saturating_add(fields[5])
            .saturating_add(fields[6])
            .saturating_add(fields[7]);
        
        let total = busy.saturating_add(fields[3])
            .saturating_add(fields[4]);
        
        result.push(CpuTimes { id, busy, total });
    }
    
    result
}

/// Użycie CPU w procentach (0-100) dla każdego wpisu z `cur`, dopasowanego po `id` do `prev`.
/// Brak odpowiednika w `prev` lub zerowy przyrost czasu => 0.0.
pub fn cpu_percent(prev: &[CpuTimes], cur: &[CpuTimes]) -> Vec<f32> {
    cur.iter()
        .map(|cur_entry| {
            // Find corresponding entry in prev
            let prev_entry = prev.iter().find(|prev_entry| prev_entry.id == cur_entry.id);
            
            match prev_entry {
                Some(prev_entry) => {
                    // Calculate deltas
                    let busy_delta = cur_entry.busy.saturating_sub(prev_entry.busy);
                    let total_delta = cur_entry.total.saturating_sub(prev_entry.total);
                    
                    if total_delta == 0 {
                        0.0
                    } else {
                        let percent = 100.0 * (busy_delta as f64) / (total_delta as f64);
                        // Clamp to 0..=100 range
                        percent.clamp(0.0, 100.0) as f32
                    }
                }
                None => 0.0,
            }
        })
        .collect()
}

pub fn parse_meminfo(text: &str) -> MemInfo {
    let mut mem_info = MemInfo::default();
    
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            
            // Parse the numeric value and unit (kB)
            if let Some(kb_str) = value.strip_suffix(" kB") {
                if let Ok(kb) = kb_str.trim().parse::<u64>() {
                    match key {
                        "MemTotal" => mem_info.total_kb = kb,
                        "MemAvailable" => mem_info.available_kb = kb,
                        "Cached" => mem_info.cached_kb = kb,
                        "SwapTotal" => mem_info.swap_total_kb = kb,
                        "SwapFree" => mem_info.swap_free_kb = kb,
                        _ => {}
                    }
                }
            }
        }
    }
    
    mem_info
}

/// Tylko całe dyski (nvme0n1, sda, vda): bez partycji, loop, ram, dm, md.
pub fn parse_diskstats(text: &str) -> Vec<DiskCounters> {
    let mut result = Vec::new();
    
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        
        // Need at least 10 fields
        if parts.len() < 10 {
            continue;
        }
        
        // Parse name (field 2)
        let name = parts[2].to_string();
        
        // Check if this is a whole device (not a partition or special device)
        if !is_whole_device(&name) {
            continue;
        }
        
        // Parse sectors_read (field 5) and sectors_written (field 9)
        if let (Some(read), Some(write)) = (
            parts[5].parse::<u64>().ok(),
            parts[9].parse::<u64>().ok()
        ) {
            result.push(DiskCounters {
                name,
                sectors_read: read,
                sectors_written: write,
            });
        }
    }
    
    result
}

/// Przepustowość dysków w MB/s (1 MB = 10^6 B, sektor = 512 B). Licznik, który zmalał, daje 0.
pub fn disk_rates(prev: &[DiskCounters], cur: &[DiskCounters], dt_s: f64) -> Vec<DiskRate> {
    if dt_s <= 0.0 || !dt_s.is_finite() {
        return vec![DiskRate { name: String::new(), read_mb_s: 0.0, write_mb_s: 0.0 }; cur.len()];
    }
    
    cur.iter()
        .map(|cur_entry| {
            // Find corresponding entry in prev
            let prev_entry = prev.iter().find(|prev_entry| prev_entry.name == cur_entry.name);
            
            let (read_mb_s, write_mb_s) = match prev_entry {
                Some(prev_entry) => {
                    // Calculate deltas
                    let read_delta = cur_entry.sectors_read.saturating_sub(prev_entry.sectors_read);
                    
                    let write_delta = cur_entry.sectors_written.saturating_sub(prev_entry.sectors_written);
                    
                    // Convert sectors to MB/s (1 sector = 512 bytes)
                    let read_mb_s = (read_delta * 512) as f64 / (1_000_000.0 * dt_s);
                    let write_mb_s = (write_delta * 512) as f64 / (1_000_000.0 * dt_s);
                    
                    (read_mb_s, write_mb_s)
                }
                None => (0.0, 0.0),
            };
            
            DiskRate {
                name: cur_entry.name.clone(),
                read_mb_s,
                write_mb_s,
            }
        })
        .collect()
}

/// Interfejsy sieciowe z `/proc/net/dev` bez `lo`.
pub fn parse_net_dev(text: &str) -> Vec<NetCounters> {
    let mut result = Vec::new();
    let mut lines = text.lines().peekable();
    
    // Skip first two header lines
    lines.next();
    lines.next();
    
    for line in lines {
        if !line.contains(':') {
            continue;
        }
        
        // Split on the first colon to separate interface name from data
        if let Some((iface, data)) = line.split_once(':') {
            let iface = iface.trim().to_string();
            
            // Skip loopback interface
            if iface == "lo" {
                continue;
            }
            
            // Parse data fields (whitespace-separated)
            let fields: Vec<&str> = data.split_whitespace().collect();
            
            // Need at least 9 fields for rx_bytes and tx_bytes
            if fields.len() < 9 {
                continue;
            }
            
            // Parse rx_bytes (field 0) and tx_bytes (field 8)
            if let (Some(rx), Some(tx)) = (
                fields[0].parse::<u64>().ok(),
                fields[8].parse::<u64>().ok()
            ) {
                result.push(NetCounters {
                    iface,
                    rx_bytes: rx,
                    tx_bytes: tx,
                });
            }
        }
    }
    
    result
}

/// Przepustowość w Mb/s (bity, 10^6). Licznik, który zmalał, daje 0.
pub fn net_rates(prev: &[NetCounters], cur: &[NetCounters], dt_s: f64) -> Vec<NetRate> {
    if dt_s <= 0.0 || !dt_s.is_finite() {
        return vec![NetRate { iface: String::new(), rx_mbit_s: 0.0, tx_mbit_s: 0.0 }; cur.len()];
    }
    
    cur.iter()
        .map(|cur_entry| {
            // Find corresponding entry in prev
            let prev_entry = prev.iter().find(|prev_entry| prev_entry.iface == cur_entry.iface);
            
            let (rx_mbit_s, tx_mbit_s) = match prev_entry {
                Some(prev_entry) => {
                    // Calculate deltas
                    let rx_delta = cur_entry.rx_bytes.saturating_sub(prev_entry.rx_bytes);
                    
                    let tx_delta = cur_entry.tx_bytes.saturating_sub(prev_entry.tx_bytes);
                    
                    // Convert bytes to Mb/s (1 byte = 8 bits, 1 Mb = 10^6 bits)
                    let rx_mbit_s = (rx_delta * 8) as f64 / (1_000_000.0 * dt_s);
                    let tx_mbit_s = (tx_delta * 8) as f64 / (1_000_000.0 * dt_s);
                    
                    (rx_mbit_s, tx_mbit_s)
                }
                None => (0.0, 0.0),
            };
            
            NetRate {
                iface: cur_entry.iface.clone(),
                rx_mbit_s,
                tx_mbit_s,
            }
        })
        .collect()
}

/// Temperatura z pliku sysfs w milistopniach ("45000\n" => 45.0).
pub fn parse_millidegrees(text: &str) -> Option<f32> {
    text.trim()
        .parse::<i64>()
        .ok()
        .map(|millidegrees| millidegrees as f32 / 1000.0)
}

/// Check if a device name represents a whole disk (not a partition or special device)
fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn all_letters(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase())
}

/// Całe dyski: `nvme<N>n<M>`, `sd<litery>`, `vd<litery>`, `xvd<litery>`; bez partycji i urządzeń wirtualnych.
fn is_whole_device(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("nvme") {
        return match rest.split_once('n') {
            Some((ctrl, ns)) => all_digits(ctrl) && all_digits(ns),
            None => false,
        };
    }
    for prefix in ["xvd", "sd", "vd"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return all_letters(rest);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 60 0 30 400 10 0 0 0 0 0\ncpu1 40 0 20 400 40 0 0 0 0 0\nintr 12345 1 2\nctxt 999\n";

    #[test]
    fn cpu_stat_parses_aggregate_and_cores() {
        let v = parse_cpu_stat(STAT);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], CpuTimes { id: None, busy: 150, total: 1000 });
        assert_eq!(v[1], CpuTimes { id: Some(0), busy: 90, total: 500 });
        assert_eq!(v[2], CpuTimes { id: Some(1), busy: 60, total: 500 });
    }

    #[test]
    fn cpu_stat_busy_includes_irq_softirq_steal_but_not_iowait() {
        let v = parse_cpu_stat("cpu 1 2 3 40 5 6 7 8\n");
        assert_eq!(v[0].busy, 1 + 2 + 3 + 6 + 7 + 8);
        assert_eq!(v[0].total, v[0].busy + 40 + 5);
    }

    #[test]
    fn cpu_percent_from_two_samples() {
        let prev = parse_cpu_stat(STAT);
        let cur = parse_cpu_stat("cpu  200 0 100 850 50 0 0 0 0 0\ncpu0 160 0 80 410 10 0 0 0 0 0\ncpu1 40 0 20 440 40 0 0 0 0 0\n");
        let p = cpu_percent(&prev, &cur);
        assert_eq!(p.len(), 3);
        assert!((p[0] - 75.0).abs() < 0.01, "zbiorczo: {}", p[0]); // 150 busy z 200
        assert!((p[1] - 93.75).abs() < 0.01, "cpu0: {}", p[1]); // busy 90->240 (+150), total 500->660 (+160)
        assert!(p[2].abs() < 0.01, "cpu1 bezczynny: {}", p[2]);
    }

    #[test]
    fn cpu_percent_handles_zero_delta_and_missing_previous() {
        let s = parse_cpu_stat(STAT);
        assert!(cpu_percent(&s, &s).iter().all(|&x| x == 0.0));
        assert!(cpu_percent(&[], &s).iter().all(|&x| x == 0.0));
        assert_eq!(cpu_percent(&s, &[]).len(), 0);
    }

    #[test]
    fn meminfo_parses_needed_fields_and_defaults_missing() {
        let t = "MemTotal:       63000000 kB\nMemFree:         1000000 kB\nMemAvailable:   60000000 kB\nBuffers:          100000 kB\nCached:          2000000 kB\nSwapTotal:       8000000 kB\nSwapFree:        7000000 kB\n";
        assert_eq!(parse_meminfo(t), MemInfo { total_kb: 63_000_000, available_kb: 60_000_000, cached_kb: 2_000_000, swap_total_kb: 8_000_000, swap_free_kb: 7_000_000 });
        assert_eq!(parse_meminfo("MemTotal: 5 kB\njunk\n"), MemInfo { total_kb: 5, ..Default::default() });
        assert_eq!(parse_meminfo(""), MemInfo::default());
    }

    const DISK: &str = "\
 259 0 nvme0n1 100 0 2048 50 200 0 4096 80 0 100 130\n\
 259 1 nvme0n1p1 10 0 100 5 20 0 200 8 0 10 13\n\
   8 0 sda 5 0 1000 5 6 0 2000 8 0 10 13\n\
   8 1 sda1 5 0 900 5 6 0 1900 8 0 10 13\n\
   7 0 loop0 1 0 8 0 0 0 0 0 0 0 0\n\
   9 0 md0 1 0 8 0 0 0 0 0 0 0 0\n\
 252 0 dm-0 1 0 8 0 0 0 0 0 0 0 0\n\
 garbage line\n";

    #[test]
    fn diskstats_keeps_only_whole_devices() {
        let v = parse_diskstats(DISK);
        let names: Vec<&str> = v.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["nvme0n1", "sda"]);
        assert_eq!((v[0].sectors_read, v[0].sectors_written), (2048, 4096));
    }

    #[test]
    fn disk_rates_convert_sectors_to_megabytes_per_second() {
        let prev = vec![DiskCounters { name: "nvme0n1".into(), sectors_read: 0, sectors_written: 1000 }];
        let cur = vec![DiskCounters { name: "nvme0n1".into(), sectors_read: 1_953_125, sectors_written: 1000 }];
        let r = disk_rates(&prev, &cur, 1.0);
        assert!((r[0].read_mb_s - 1000.0).abs() < 0.01, "{}", r[0].read_mb_s); // 1953125*512 = 1e9 B
        assert_eq!(r[0].write_mb_s, 0.0);
        let half = disk_rates(&prev, &cur, 2.0);
        assert!((half[0].read_mb_s - 500.0).abs() < 0.01);
    }

    #[test]
    fn disk_rates_survive_counter_reset_and_bad_dt() {
        let prev = vec![DiskCounters { name: "a".into(), sectors_read: 500, sectors_written: 500 }];
        let cur = vec![DiskCounters { name: "a".into(), sectors_read: 10, sectors_written: 10 }];
        let r = disk_rates(&prev, &cur, 1.0);
        assert_eq!((r[0].read_mb_s, r[0].write_mb_s), (0.0, 0.0));
        assert_eq!(disk_rates(&cur, &prev, 0.0)[0].read_mb_s, 0.0, "dt = 0 nie może dać inf/NaN");
        assert_eq!(disk_rates(&cur, &prev, -1.0)[0].read_mb_s, 0.0);
    }

    const NET: &str = "\
Inter-|   Receive                                                |  Transmit\n\
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n\
    lo: 1000 10 0 0 0 0 0 0 1000 10 0 0 0 0 0 0\n\
enp4s0: 5000000 100 0 0 0 0 0 0 2000000 80 0 0 0 0 0 0\n\
wlan0:123 1 0 0 0 0 0 0 456 2 0 0 0 0 0 0\n";

    #[test]
    fn net_dev_skips_loopback_and_handles_missing_space_after_colon() {
        let v = parse_net_dev(NET);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].iface.as_str(), v[0].rx_bytes, v[0].tx_bytes), ("enp4s0", 5_000_000, 2_000_000));
        assert_eq!((v[1].iface.as_str(), v[1].rx_bytes, v[1].tx_bytes), ("wlan0", 123, 456));
    }

    #[test]
    fn net_rates_in_megabits() {
        let prev = vec![NetCounters { iface: "e".into(), rx_bytes: 0, tx_bytes: 100 }];
        let cur = vec![NetCounters { iface: "e".into(), rx_bytes: 125_000_000, tx_bytes: 100 }];
        let r = net_rates(&prev, &cur, 1.0);
        assert!((r[0].rx_mbit_s - 1000.0).abs() < 0.01, "{}", r[0].rx_mbit_s);
        assert_eq!(r[0].tx_mbit_s, 0.0);
        assert_eq!(net_rates(&cur, &prev, 1.0)[0].rx_mbit_s, 0.0, "licznik zmalał");
    }

    #[test]
    fn millidegrees_parsing() {
        assert_eq!(parse_millidegrees("45000\n"), Some(45.0));
        assert_eq!(parse_millidegrees(" 38500 "), Some(38.5));
        assert_eq!(parse_millidegrees("abc"), None);
        assert_eq!(parse_millidegrees(""), None);
    }

    #[test]
    fn garbage_input_never_panics() {
        for g in ["", "\n\n", "cpu", "cpu x y z", "cpuX 1 2 3 4", "cpu 99999999999999999999999 1 1 1 1", ":::", "a:b:c"] {
            let _ = parse_cpu_stat(g);
            let _ = parse_meminfo(g);
            let _ = parse_diskstats(g);
            let _ = parse_net_dev(g);
            let _ = parse_millidegrees(g);
        }
    }
}
