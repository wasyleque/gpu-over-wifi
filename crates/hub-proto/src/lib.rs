//! Wspólne typy protokołu GPU Hub (serwer ↔ klient). Transport: JSON przez HTTP.

use serde::{Deserialize, Serialize};

/// Wersja protokołu; klient odrzuca serwer o innej wersji głównej.
pub const PROTOCOL_VERSION: u32 = 1;

/// Nagłówek z tokenem autoryzacji (`Authorization: Bearer <token>`).
pub const AUTH_HEADER: &str = "authorization";

/// Nazwa usługi mDNS, pod którą serwer się ogłasza.
pub const MDNS_SERVICE: &str = "_gpuhub._tcp.local.";

/// Domyślny port API huba.
pub const DEFAULT_PORT: u16 = 7878;

/// Stan jednej karty graficznej.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuStatus {
    pub index: u32,
    pub name: String,
    pub vram_total_mib: u64,
    pub vram_used_mib: u64,
    pub temperature_c: u32,
    /// Obciążenie GPU w procentach (0–100).
    pub utilization_pct: u32,
}

/// Model załadowany w backendzie AI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadedModel {
    pub name: String,
    pub backend: Backend,
    pub vram_mib: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Ollama,
    Inferssd,
}

/// Stan schedulera zadań AI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulerStatus {
    pub queue_len: u32,
    pub active_backend: Option<String>,
    pub switches_total: u64,
    pub in_flight: u32,
}

/// Odpowiedź endpointu `GET /status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusResponse {
    pub protocol_version: u32,
    pub hostname: String,
    pub hub_version: String,
    pub gpus: Vec<GpuStatus>,
    pub models: Vec<LoadedModel>,
    #[serde(default)]
    pub scheduler: Option<SchedulerStatus>,
}

/// Informacje zwracane przy wykrywaniu serwera (mDNS TXT / `GET /hello`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub hostname: String,
    pub port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StatusResponse {
        StatusResponse {
            protocol_version: PROTOCOL_VERSION,
            hostname: "wasyl-srv".into(),
            hub_version: "0.1.0".into(),
            gpus: vec![GpuStatus {
                index: 0,
                name: "NVIDIA GeForce RTX 3090".into(),
                vram_total_mib: 24576,
                vram_used_mib: 1024,
                temperature_c: 45,
                utilization_pct: 3,
            }],
            models: vec![LoadedModel {
                name: "qwen".into(),
                backend: Backend::Ollama,
                vram_mib: 8000,
            }],
            scheduler: None,
        }
    }

    #[test]
    fn status_roundtrip() {
        let s = sample();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<StatusResponse>(&json).unwrap(), s);
    }

    #[test]
    fn legacy_status_without_scheduler_deserializes() {
        let legacy_json = r#"{
            "protocol_version": 1,
            "hostname": "wasyl-srv",
            "hub_version": "0.1.0",
            "gpus": [],
            "models": []
        }"#;
        let parsed: StatusResponse = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(parsed.scheduler, None);
    }

    #[test]
    fn backend_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Backend::Inferssd).unwrap(), "\"inferssd\"");
    }
}
