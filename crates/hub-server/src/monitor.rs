//! Monitor obciążenia na żywo: próbnik systemu i GPU, historia w pamięci oraz widok telemetrii
//! InferSSD (skąd pochodzą eksperty: GPU / RAM / SSD). Dane trafiają do strony `/monitor`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nvml_wrapper::{enum_wrappers::device::{Clock, TemperatureSensor}, Nvml};
use serde::Serialize;

use crate::scheduler::Scheduler;
use crate::sysmon::{self, CpuTimes, DiskCounters, DiskRate, NetCounters, NetRate};
use crate::tap::{AnswerStats, Tap, TokenStats, TailReader};

/// Ile próbek (1 próbka = 1 s) trzymamy w pamięci.
pub const HISTORY_LEN: usize = 900;

#[derive(Debug, Clone, Serialize)]
pub struct GpuSample {
    pub index: u32,
    pub name: String,
    pub util_pct: u32,
    pub mem_util_pct: u32,
    pub vram_used_mib: u64,
    pub vram_total_mib: u64,
    pub temp_c: u32,
    pub power_w: f32,
    pub power_limit_w: f32,
    pub fan_pct: Option<u32>,
    pub sm_clock_mhz: u32,
    pub mem_clock_mhz: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    /// Czas unixowy w sekundach.
    pub t: f64,
    pub gpus: Vec<GpuSample>,
    pub cpu_pct: f32,
    pub cpu_cores_pct: Vec<f32>,
    pub cpu_temp_c: Option<f32>,
    pub load1: f32,
    pub ram_used_mib: u64,
    pub ram_total_mib: u64,
    pub ram_cached_mib: u64,
    pub swap_used_mib: u64,
    pub disks: Vec<DiskRate>,
    pub net: Vec<NetRate>,
    pub queue_len: u32,
    pub in_flight: u32,
    pub active_backend: Option<String>,
    pub switches_total: u64,
    /// Modele załadowane w VRAM (wszystkie backendy).
    pub loaded: Vec<hub_proto::LoadedModel>,
}

/// Widok telemetrii wnioskowania dla strony.
#[derive(Debug, Clone, Serialize)]
pub struct InferenceView {
    pub layers: usize,
    pub experts: usize,
    /// Cyfry '0'..'3' (TIER_*), warstwa po warstwie; długość = layers * experts.
    pub heat: String,
    pub picked: Vec<[u16; 2]>,
    pub current: TokenView,
    pub last: Option<TokenView>,
    pub answer: AnswerView,
    pub tokens_per_sec: f64,
    pub running: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct TokenView {
    pub token: u64,
    pub experts: u32,
    pub gpu: u32,
    pub ram: u32,
    pub ssd: u32,
    pub read_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct AnswerView {
    pub tokens: u32,
    pub experts: u64,
    pub gpu: u64,
    pub ram: u64,
    pub ssd: u64,
    pub read_bytes: u64,
}

impl From<TokenStats> for TokenView {
    fn from(t: TokenStats) -> Self {
        Self { token: t.token, experts: t.experts, gpu: t.gpu, ram: t.ram, ssd: t.ssd, read_bytes: t.read_bytes }
    }
}

impl From<AnswerStats> for AnswerView {
    fn from(a: AnswerStats) -> Self {
        Self { tokens: a.tokens, experts: a.experts, gpu: a.gpu, ram: a.ram, ssd: a.ssd, read_bytes: a.read_bytes }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub size_bytes: u64,
}

pub struct Monitor {
    pub host: String,
    pub model: ModelInfo,
    history: Mutex<VecDeque<Sample>>,
    tap: Mutex<Tap>,
    /// Czy kiedykolwiek napłynęły zdarzenia telemetrii (bez tego widok wnioskowania jest ukryty).
    fed: AtomicBool,
}

pub fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl Monitor {
    pub fn new(host: String, model: ModelInfo, layers: usize, experts: usize) -> Arc<Self> {
        Arc::new(Self {
            host,
            model,
            history: Mutex::new(VecDeque::with_capacity(HISTORY_LEN)),
            tap: Mutex::new(Tap::new(layers, experts)),
            fed: AtomicBool::new(false),
        })
    }

    pub fn push(&self, s: Sample) {
        if let Ok(mut h) = self.history.lock() {
            if h.len() >= HISTORY_LEN {
                h.pop_front();
            }
            h.push_back(s);
        }
    }

    /// Próbki nowsze niż `since` (czas unixowy).
    pub fn since(&self, since: f64) -> Vec<Sample> {
        self.history
            .lock()
            .map(|h| h.iter().filter(|s| s.t > since).cloned().collect())
            .unwrap_or_default()
    }

    pub fn feed_tap(&self, lines: &[String], now: f64) {
        if !lines.is_empty() {
            self.fed.store(true, Ordering::Relaxed);
        }
        if let Ok(mut t) = self.tap.lock() {
            for l in lines {
                t.feed_line(l, now);
            }
        }
    }

    pub fn clear_tap(&self) {
        if let Ok(mut t) = self.tap.lock() {
            t.clear();
        }
    }

    pub fn inference(&self, now: f64) -> Option<InferenceView> {
        if !self.fed.load(Ordering::Relaxed) {
            return None;
        }
        let snap = self.tap.lock().ok()?.snapshot(now);
        Some(InferenceView {
            layers: snap.layers,
            experts: snap.experts,
            heat: snap.heat.iter().map(|&h| char::from(b'0' + h.min(9))).collect(),
            picked: snap.picked.iter().map(|&(l, e)| [l, e]).collect(),
            current: snap.current.into(),
            last: snap.last.map(Into::into),
            answer: snap.answer.into(),
            tokens_per_sec: snap.tokens_per_sec,
            running: snap.running,
        })
    }

    /// Uruchamia w tle próbnik systemu (1 Hz) i czytnik telemetrii InferSSD (4 Hz).
    pub fn spawn(self: &Arc<Self>, nvml: Option<Arc<Nvml>>, scheduler: Arc<Scheduler>) {
        let me = self.clone();
        let sched = scheduler.clone();
        tokio::spawn(async move {
            let mut prev = Prev::default();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let s = collect_sample(nvml.as_deref(), &sched, &mut prev).await;
                me.push(s);
            }
        });
        let me = self.clone();
        tokio::spawn(async move {
            let mut reader = TailReader::new(PathBuf::new());
            let mut current: Option<PathBuf> = None;
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tick.tick().await;
                let path = scheduler.telemetry_path();
                if path != current {
                    if let Some(p) = &path {
                        reader.set_path(p.clone());
                    }
                    me.clear_tap();
                    current = path;
                }
                if current.is_some() {
                    let lines = reader.read_new_lines();
                    if !lines.is_empty() {
                        me.feed_tap(&lines, unix_now());
                    }
                }
            }
        });
    }
}

#[derive(Default)]
struct Prev {
    cpu: Vec<CpuTimes>,
    disks: Vec<DiskCounters>,
    net: Vec<NetCounters>,
    at: Option<f64>,
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

async fn collect_sample(nvml: Option<&Nvml>, sched: &Arc<Scheduler>, prev: &mut Prev) -> Sample {
    let now = unix_now();
    let dt = prev.at.map(|p| now - p).unwrap_or(0.0);

    let cpu = sysmon::parse_cpu_stat(&read("/proc/stat"));
    let pct = sysmon::cpu_percent(&prev.cpu, &cpu);
    let (cpu_pct, cores): (f32, Vec<f32>) = match cpu.first() {
        Some(t) if t.id.is_none() => (pct.first().copied().unwrap_or(0.0), pct.iter().skip(1).copied().collect()),
        _ => (0.0, pct.clone()),
    };
    let disks_now = sysmon::parse_diskstats(&read("/proc/diskstats"));
    let net_now = sysmon::parse_net_dev(&read("/proc/net/dev"));
    let disks = sysmon::disk_rates(&prev.disks, &disks_now, dt);
    let net = sysmon::net_rates(&prev.net, &net_now, dt);
    prev.cpu = cpu;
    prev.disks = disks_now;
    prev.net = net_now;
    prev.at = Some(now);

    let mem = sysmon::parse_meminfo(&read("/proc/meminfo"));
    let load1 = read("/proc/loadavg").split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let cpu_temp_c = cpu_temperature();
    let st = sched.status().await;

    Sample {
        t: now,
        gpus: nvml.map(gpu_samples).unwrap_or_default(),
        cpu_pct,
        cpu_cores_pct: cores,
        cpu_temp_c,
        load1,
        ram_used_mib: mem.total_kb.saturating_sub(mem.available_kb) / 1024,
        ram_total_mib: mem.total_kb / 1024,
        ram_cached_mib: mem.cached_kb / 1024,
        swap_used_mib: mem.swap_total_kb.saturating_sub(mem.swap_free_kb) / 1024,
        disks,
        net,
        queue_len: st.queue_len,
        in_flight: st.in_flight,
        active_backend: st.active_backend,
        switches_total: st.switches_total,
        loaded: sched.all_loaded().await,
    }
}

/// Najwyższa temperatura z czujników `coretemp`/`k10temp`/`zenpower` w `/sys/class/hwmon`.
fn cpu_temperature() -> Option<f32> {
    let mut best: Option<f32> = None;
    for entry in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let dir = entry.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
        if !matches!(name.trim(), "coretemp" | "k10temp" | "zenpower") {
            continue;
        }
        for n in 1..=16 {
            if let Some(t) = sysmon::parse_millidegrees(&read(&dir.join(format!("temp{n}_input")).to_string_lossy())) {
                best = Some(best.map_or(t, |b| b.max(t)));
            }
        }
    }
    best
}

fn gpu_samples(nvml: &Nvml) -> Vec<GpuSample> {
    let count = nvml.device_count().unwrap_or(0);
    let mut out = Vec::new();
    for index in 0..count {
        let Ok(d) = nvml.device_by_index(index) else { continue };
        let util = d.utilization_rates().ok();
        let mem = d.memory_info().ok();
        out.push(GpuSample {
            index,
            name: d.name().unwrap_or_default(),
            util_pct: util.as_ref().map_or(0, |u| u.gpu),
            mem_util_pct: util.as_ref().map_or(0, |u| u.memory),
            vram_used_mib: mem.as_ref().map_or(0, |m| m.used / (1024 * 1024)),
            vram_total_mib: mem.as_ref().map_or(0, |m| m.total / (1024 * 1024)),
            temp_c: d.temperature(TemperatureSensor::Gpu).unwrap_or(0),
            power_w: d.power_usage().map_or(0.0, |mw| mw as f32 / 1000.0),
            power_limit_w: d.enforced_power_limit().map_or(0.0, |mw| mw as f32 / 1000.0),
            fan_pct: d.fan_speed(0).ok(),
            sm_clock_mhz: d.clock_info(Clock::SM).unwrap_or(0),
            mem_clock_mhz: d.clock_info(Clock::Memory).unwrap_or(0),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t: f64) -> Sample {
        Sample {
            t, gpus: vec![], cpu_pct: 0.0, cpu_cores_pct: vec![], cpu_temp_c: None, load1: 0.0,
            ram_used_mib: 0, ram_total_mib: 0, ram_cached_mib: 0, swap_used_mib: 0, disks: vec![], net: vec![],
            queue_len: 0, in_flight: 0, active_backend: None, switches_total: 0, loaded: vec![],
        }
    }

    fn monitor() -> Arc<Monitor> {
        Monitor::new("h".into(), ModelInfo { name: "m".into(), size_bytes: 1 }, 2, 4)
    }

    #[test]
    fn history_is_a_bounded_ring_and_since_filters_by_time() {
        let m = monitor();
        for i in 0..(HISTORY_LEN + 50) {
            m.push(sample(i as f64));
        }
        let all = m.since(-1.0);
        assert_eq!(all.len(), HISTORY_LEN);
        assert_eq!(all[0].t, 50.0, "najstarsze próbki wypadają");
        let newer = m.since((HISTORY_LEN + 47) as f64);
        assert_eq!(newer.len(), 2);
    }

    #[test]
    fn inference_view_encodes_heat_as_digits() {
        let m = monitor();
        assert!(m.inference(0.0).is_none(), "bez zdarzeń widok wnioskowania jest ukryty");
        let line = r#"{"ev":"ssd.place.layer","decode":true,"layer":1,"token":0,"selected_experts":[0,3],"vram_hit":[true,false],"cache_hit":[false,true],"read_bytes":5}"#;
        m.feed_tap(&[line.to_string()], 100.0);
        let v = m.inference(100.5).unwrap();
        assert_eq!((v.layers, v.experts, v.heat.len()), (2, 4, 8));
        assert_eq!(v.heat, "00003002", "warstwa 1: ekspert 0 = GPU(3), ekspert 3 = RAM(2)");
        assert_eq!(v.picked.len(), 2);
        assert!(v.running);
        assert_eq!(v.current.read_bytes, 5);
        m.clear_tap();
        assert_eq!(m.inference(100.5).unwrap().heat, "00000000");
    }

    #[test]
    fn sample_serializes_to_json() {
        let j = serde_json::to_string(&sample(1.5)).unwrap();
        assert!(j.contains("\"t\":1.5") && j.contains("\"gpus\":[]"));
    }
}
