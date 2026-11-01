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

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, PartialEq)]
pub struct NetRate {
    pub iface: String,
    pub rx_mbit_s: f64,
    pub tx_mbit_s: f64,
}

/// Linie `cpu` i `cpuN` z `/proc/stat` (zbiorcza pierwsza, jeśli jest w pliku).
pub fn parse_cpu_stat(text: &str) -> Vec<CpuTimes> {
    todo!()
}

/// Użycie CPU w procentach (0-100) dla każdego wpisu z `cur`, dopasowanego po `id` do `prev`.
/// Brak odpowiednika w `prev` lub zerowy przyrost czasu => 0.0.
pub fn cpu_percent(prev: &[CpuTimes], cur: &[CpuTimes]) -> Vec<f32> {
    todo!()
}

pub fn parse_meminfo(text: &str) -> MemInfo {
    todo!()
}

/// Tylko całe dyski (nvme0n1, sda, vda): bez partycji, loop, ram, dm, md.
pub fn parse_diskstats(text: &str) -> Vec<DiskCounters> {
    todo!()
}

/// Przepustowość dysków w MB/s (1 MB = 10^6 B, sektor = 512 B). Licznik, który zmalał, daje 0.
pub fn disk_rates(prev: &[DiskCounters], cur: &[DiskCounters], dt_s: f64) -> Vec<DiskRate> {
    todo!()
}

/// Interfejsy sieciowe z `/proc/net/dev` bez `lo`.
pub fn parse_net_dev(text: &str) -> Vec<NetCounters> {
    todo!()
}

/// Przepustowość w Mb/s (bity, 10^6). Licznik, który zmalał, daje 0.
pub fn net_rates(prev: &[NetCounters], cur: &[NetCounters], dt_s: f64) -> Vec<NetRate> {
    todo!()
}

/// Temperatura z pliku sysfs w milistopniach ("45000\n" => 45.0).
pub fn parse_millidegrees(text: &str) -> Option<f32> {
    todo!()
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
