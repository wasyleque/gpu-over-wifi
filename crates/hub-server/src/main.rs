use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use clap::Parser;
use hub_proto::{
    Backend, GpuStatus, Hello, LoadedModel, StatusResponse, AUTH_HEADER, DEFAULT_PORT,
    MDNS_SERVICE, PROTOCOL_VERSION,
};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use nvml_wrapper::{enum_wrappers::device::TemperatureSensor, Nvml};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tracing::{error, info, warn};

mod backend;
mod inferssd;
mod proxy;
mod scheduler;

/// Domyślny adres API Ollamy.
const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

#[derive(Parser, Debug)]
#[command(name = "hub-server", version, about = "Demon GPU Hub")]
struct Args {
    /// Adres i port nasłuchiwania HTTP
    #[arg(long, env = "HUB_LISTEN", default_value_t = default_listen_addr())]
    listen: SocketAddr,

    /// Token autoryzacji Bearer
    #[arg(long, env = "HUB_TOKEN")]
    token: Option<String>,

    /// Ścieżka do pliku z tokenem autoryzacji Bearer
    #[arg(long, env = "HUB_TOKEN_FILE")]
    token_file: Option<PathBuf>,

    /// Bazowy URL lokalnej instancji Ollamy
    #[arg(long, env = "HUB_OLLAMA_URL", default_value = DEFAULT_OLLAMA_URL)]
    ollama_url: String,

    /// Limit rozmiaru body żądania w MiB
    #[arg(long, env = "HUB_MAX_BODY_MIB", default_value_t = proxy::DEFAULT_MAX_BODY_MIB)]
    max_body_mib: usize,

    /// Plik konfiguracyjny `hub.toml` (scheduler i backendy); flaga `--ollama-url` nadal obowiązuje bez niego
    #[arg(long, env = "HUB_CONFIG")]
    config: Option<PathBuf>,
}

fn default_listen_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT))
}

#[derive(Clone)]
pub struct AppState {
    pub token: String,
    pub hostname: String,
    pub port: u16,
    pub ollama_url: String,
    pub http_client: reqwest::Client,
    pub proxy_client: reqwest::Client,
    pub max_body_mib: usize,
    pub scheduler: Arc<scheduler::Scheduler>,
    pub nvml: Option<Arc<Nvml>>,
}

#[derive(Debug, Deserialize)]
struct OllamaPsResponse {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaModel {
    name: String,
    #[serde(default)]
    size_vram: u64,
}

/// Parsuje odpowiedź JSON z `GET /api/ps` Ollamy na listę `LoadedModel`.
pub fn parse_ollama_ps(json_str: &str) -> Result<Vec<LoadedModel>, serde_json::Error> {
    let resp: OllamaPsResponse = serde_json::from_str(json_str)?;
    Ok(resp
        .models
        .into_iter()
        .map(|m| LoadedModel {
            name: m.name,
            backend: Backend::Ollama,
            vram_mib: m.size_vram / (1024 * 1024),
        })
        .collect())
}

/// Sprawdza w stałym czasie, czy nagłówek `Authorization` zawiera poprawny token Bearer.
pub fn verify_bearer_token(headers: &HeaderMap, expected_token: &str) -> bool {
    let Some(header_val) = headers.get(AUTH_HEADER) else {
        return false;
    };
    let Ok(header_str) = header_val.to_str() else {
        return false;
    };
    let Some((scheme, provided_token)) = header_str.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return false;
    }
    constant_time_str_eq(provided_token.as_bytes(), expected_token.as_bytes())
}

fn constant_time_str_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Zbiera status wszystkich kart GPU za pomocą NVML.
fn collect_gpu_status(nvml: Option<&Nvml>) -> Vec<GpuStatus> {
    let Some(nvml) = nvml else {
        return Vec::new();
    };

    let count = match nvml.device_count() {
        Ok(c) => c,
        Err(e) => {
            warn!("Nie udało się pobrać liczby urządzeń NVML: {e}");
            return Vec::new();
        }
    };

    let mut gpus = Vec::with_capacity(count as usize);
    for index in 0..count {
        match nvml.device_by_index(index) {
            Ok(device) => {
                let name = device.name().unwrap_or_else(|e| {
                    warn!("Błąd odczytu nazwy GPU {index}: {e}");
                    "Unknown GPU".to_string()
                });
                let (vram_total_mib, vram_used_mib) = match device.memory_info() {
                    Ok(mem) => (mem.total / (1024 * 1024), mem.used / (1024 * 1024)),
                    Err(e) => {
                        warn!("Błąd odczytu pamięci GPU {index}: {e}");
                        (0, 0)
                    }
                };
                let temperature_c = device
                    .temperature(TemperatureSensor::Gpu)
                    .unwrap_or_else(|e| {
                        warn!("Błąd odczytu temperatury GPU {index}: {e}");
                        0
                    });
                let utilization_pct = device
                    .utilization_rates()
                    .map(|u| u.gpu)
                    .unwrap_or_else(|e| {
                        warn!("Błąd odczytu obciążenia GPU {index}: {e}");
                        0
                    });

                gpus.push(GpuStatus {
                    index,
                    name,
                    vram_total_mib,
                    vram_used_mib,
                    temperature_c,
                    utilization_pct,
                });
            }
            Err(e) => {
                warn!("Nie udało się otworzyć urządzenia GPU {index}: {e}");
            }
        }
    }
    gpus
}

/// Pobiera listę załadowanych modeli z lokalnej Ollamy.
async fn fetch_ollama_models(client: &reqwest::Client, base_url: &str) -> Vec<LoadedModel> {
    let url = format!("{}/api/ps", base_url.trim_end_matches('/'));
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("Nie udało się połączyć z Ollamą ({url}): {e}");
            return Vec::new();
        }
    };

    if !resp.status().is_success() {
        warn!("Ollama zwróciła błąd HTTP {} dla {url}", resp.status());
        return Vec::new();
    }

    let body = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            warn!("Błąd odczytu odpowiedzi z Ollamy: {e}");
            return Vec::new();
        }
    };

    match parse_ollama_ps(&body) {
        Ok(models) => models,
        Err(e) => {
            warn!("Błąd parsowania odpowiedzi z Ollamy: {e}");
            Vec::new()
        }
    }
}

async fn hello_handler(State(state): State<AppState>) -> Json<Hello> {
    Json(Hello {
        protocol_version: PROTOCOL_VERSION,
        hostname: state.hostname,
        port: state.port,
    })
}

async fn status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StatusResponse>, StatusCode> {
    if !verify_bearer_token(&headers, &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let gpus = collect_gpu_status(state.nvml.as_deref());
    let mut models = fetch_ollama_models(&state.http_client, &state.ollama_url).await;
    models.extend(state.scheduler.extra_loaded_models().await);
    let scheduler = state.scheduler.status().await;

    Ok(Json(StatusResponse {
        protocol_version: PROTOCOL_VERSION,
        hostname: state.hostname,
        hub_version: env!("CARGO_PKG_VERSION").to_string(),
        gpus,
        models,
        scheduler: Some(scheduler),
    }))
}

pub fn build_router(state: AppState) -> Router {
    let max_body_mib = state.max_body_mib;
    let router = Router::new()
        .route("/hello", get(hello_handler))
        .route("/status", get(status_handler));
    proxy::register_routes(router, max_body_mib).with_state(state)
}

fn load_token(token_arg: Option<String>, token_file: Option<PathBuf>) -> Result<String, String> {
    if let Some(t) = token_arg {
        let trimmed = t.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }

    if let Some(path) = token_file {
        let content = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "Nie udało się odczytać pliku z tokenem ({}): {e}",
                path.display()
            )
        })?;
        let trimmed = content.trim().to_string();
        if trimmed.is_empty() {
            return Err(format!("Plik z tokenem ({}) jest pusty", path.display()));
        }
        return Ok(trimmed);
    }

    Err("Brak tokenu autoryzacji! Ustaw zmienną środowiskową HUB_TOKEN lub podaj --token-file.".to_string())
}

fn start_mdns(hostname: &str, port: u16) -> Option<ServiceDaemon> {
    let mdns = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("Nie udało się uruchomić demona mDNS: {e}");
            return None;
        }
    };

    let host_fqdn = format!("{hostname}.local.");
    let properties = [
        ("protocol_version", PROTOCOL_VERSION.to_string()),
        ("hostname", hostname.to_string()),
    ];

    match ServiceInfo::new(
        MDNS_SERVICE,
        hostname,
        &host_fqdn,
        "",
        port,
        &properties[..],
    ) {
        Ok(service_info) => {
            let service_info = service_info.enable_addr_auto();
            if let Err(e) = mdns.register(service_info) {
                warn!("Nie udało się zarejestrować usługi mDNS ({MDNS_SERVICE}): {e}");
            } else {
                info!("Zarejestrowano mDNS: {MDNS_SERVICE} ({hostname}:{port})");
            }
        }
        Err(e) => {
            warn!("Nie udało się utworzyć opisu usługi mDNS: {e}");
        }
    }

    Some(mdns)
}

/// Buduje konfigurację backendu InferSSD z wpisu `[[backend]]` w `hub.toml`.
fn inferssd_config(cfg: &scheduler::BackendConfig) -> inferssd::InferssdConfig {
    let port = cfg.port.unwrap_or(8080);
    inferssd::InferssdConfig {
        name: cfg.name.clone(),
        binary: PathBuf::from(cfg.binary.as_deref().unwrap_or("inferssd")),
        base_url: if cfg.url.is_empty() {
            format!("http://127.0.0.1:{port}")
        } else {
            cfg.url.clone()
        },
        port,
        models: cfg
            .model_paths
            .iter()
            .map(|(alias, path)| (alias.clone(), PathBuf::from(path)))
            .collect(),
        extra_args: cfg.args.clone(),
        log_file: cfg.log_file.as_ref().map(PathBuf::from),
        start_timeout: Duration::from_secs(cfg.start_timeout_secs.unwrap_or(1800)),
        poll_interval: Duration::from_secs(2),
        retry_after: Duration::from_secs(cfg.retry_after_secs.unwrap_or(60)),
        term_grace: Duration::from_secs(15),
    }
}

/// Inicjalizuje NVML; na Ubuntu bez pakietu -dev istnieje tylko `libnvidia-ml.so.1`.
fn init_nvml() -> Result<Nvml, nvml_wrapper::error::NvmlError> {
    Nvml::init().or_else(|_| {
        Nvml::builder()
            .lib_path(std::ffi::OsStr::new("libnvidia-ml.so.1"))
            .init()
    })
}

fn detect_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "gpu-hub".to_string())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let token = match load_token(args.token, args.token_file) {
        Ok(t) => t,
        Err(msg) => {
            error!("{msg}");
            std::process::exit(1);
        }
    };

    let nvml = match init_nvml() {
        Ok(n) => {
            info!("Zainicjalizowano NVML");
            Some(Arc::new(n))
        }
        Err(e) => {
            warn!("NVML niedostępny (lista GPU będzie pusta): {e}");
            None
        }
    };

    let hostname = detect_hostname();
    let port = args.listen.port();

    let _mdns = start_mdns(&hostname, port);

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("Błąd inicjalizacji klienta HTTP");

    let proxy_client = proxy::build_proxy_client();

    let hub_config = match &args.config {
        Some(path) => match scheduler::HubConfig::load_from_file(path) {
            Ok(c) => c,
            Err(msg) => {
                error!("{msg}");
                std::process::exit(1);
            }
        },
        None => scheduler::HubConfig::default(),
    };

    // Domyślny backend (indeks 0) to zawsze Ollama; reszta z konfiguracji idzie za nim.
    let mut backends: Vec<Arc<dyn backend::Backend>> = Vec::new();
    let mut others: Vec<Arc<dyn backend::Backend>> = Vec::new();
    for cfg in &hub_config.backends {
        match cfg.kind.as_str() {
            "ollama" => backends.push(Arc::new(backend::OllamaBackend::with_name_and_models(
                cfg.name.clone(),
                cfg.url.clone(),
                proxy_client.clone(),
                cfg.models.clone(),
            ))),
            "inferssd" => others.push(Arc::new(inferssd::InferssdBackend::new(inferssd_config(cfg)))),
            other => {
                error!("Backend '{}': nieobsługiwany rodzaj '{other}'", cfg.name);
                std::process::exit(1);
            }
        }
    }
    if backends.is_empty() {
        backends.push(Arc::new(backend::OllamaBackend::new(
            args.ollama_url.clone(),
            proxy_client.clone(),
        )));
    }
    backends.extend(others);
    let scheduler = scheduler::Scheduler::new(hub_config.scheduler, backends);
    scheduler.spawn_idle_reaper();

    let state = AppState {
        token,
        hostname,
        port,
        ollama_url: args.ollama_url,
        http_client,
        proxy_client,
        max_body_mib: args.max_body_mib,
        scheduler,
        nvml,
    };

    let app = build_router(state);

    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(l) => l,
        Err(e) => {
            error!("Nie udało się zbindować adresu {}: {e}", args.listen);
            std::process::exit(1);
        }
    };

    info!("hub-server nasłuchuje na http://{}", args.listen);
    if let Err(e) = axum::serve(listener, app).await {
        error!("Błąd serwera HTTP: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState {
            token: "secret-test-token".to_string(),
            hostname: "test-srv".to_string(),
            port: DEFAULT_PORT,
            ollama_url: "http://127.0.0.1:9".to_string(), // nieaktywny port
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_millis(100))
                .build()
                .unwrap(),
            proxy_client: proxy::build_proxy_client(),
            max_body_mib: proxy::DEFAULT_MAX_BODY_MIB,
            scheduler: scheduler::Scheduler::new(
                scheduler::SchedulerConfig::default(),
                vec![Arc::new(backend::OllamaBackend::new(
                    "http://127.0.0.1:9".to_string(),
                    reqwest::Client::new(),
                ))],
            ),
            nvml: None,
        }
    }

    #[tokio::test]
    async fn hello_works_without_auth() {
        let app = build_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/hello")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let hello: Hello = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            hello,
            Hello {
                protocol_version: PROTOCOL_VERSION,
                hostname: "test-srv".to_string(),
                port: DEFAULT_PORT,
            }
        );
    }

    #[tokio::test]
    async fn status_rejects_missing_or_invalid_token() {
        let app = build_router(test_state());

        // Brak nagłówka Authorization -> 401
        let res_no_auth = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_no_auth.status(), StatusCode::UNAUTHORIZED);

        // Zły token -> 401
        let res_bad_token = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .header(AUTH_HEADER, "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_bad_token.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn status_accepts_valid_token_and_matches_hub_proto() {
        let app = build_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .header(AUTH_HEADER, "Bearer secret-test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let status: StatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.protocol_version, PROTOCOL_VERSION);
        assert_eq!(status.hostname, "test-srv");
        assert_eq!(status.hub_version, env!("CARGO_PKG_VERSION"));
        assert!(status.gpus.is_empty());
        assert!(status.models.is_empty());
    }

    #[test]
    fn parses_ollama_ps_response() {
        let sample_json = r#"{
            "models": [
                {
                    "name": "qwen2.5-coder:32b",
                    "model": "qwen2.5-coder:32b",
                    "size": 19851340800,
                    "digest": "abc123",
                    "details": {
                        "format": "gguf",
                        "family": "qwen2",
                        "parameter_size": "32.8B",
                        "quantization_level": "Q4_K_M"
                    },
                    "expires_at": "2026-10-31T16:00:00Z",
                    "size_vram": 18874368000
                },
                {
                    "name": "nomic-embed-text:latest",
                    "size": 274302464,
                    "size_vram": 274726912
                }
            ]
        }"#;

        let models = parse_ollama_ps(sample_json).unwrap();
        assert_eq!(
            models,
            vec![
                LoadedModel {
                    name: "qwen2.5-coder:32b".to_string(),
                    backend: Backend::Ollama,
                    vram_mib: 18000,
                },
                LoadedModel {
                    name: "nomic-embed-text:latest".to_string(),
                    backend: Backend::Ollama,
                    vram_mib: 262,
                },
            ]
        );
    }

    #[test]
    fn load_token_refuses_empty() {
        assert!(load_token(None, None).is_err());
        assert!(load_token(Some("   ".to_string()), None).is_err());
        assert_eq!(
            load_token(Some(" my-secret ".to_string()), None).unwrap(),
            "my-secret"
        );
    }
}
