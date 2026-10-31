use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

/// Klasa współdzielenia VRAM przez backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ResidencyClass {
    /// Wiele mniejszych modeli naraz w ramach budżetu VRAM (np. Ollama).
    Shared,
    /// Zajmuje całą kartę na wyłączność (np. InferSSD).
    Exclusive,
}

/// Informacja o modelu dostępnym lub załadowanym w backendzie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendModelInfo {
    pub name: String,
    pub size_vram_mib: u64,
}

/// Wspólny interfejs backendu AI (Ollama, InferSSD, atrapy testowe).
#[async_trait]
pub trait Backend: Send + Sync {
    /// Nazwa identyfikująca backend (np. `"ollama"`).
    fn name(&self) -> &str;

    /// Bazowy URL HTTP backendu (np. `"http://127.0.0.1:11434"`).
    fn base_url(&self) -> &str;

    /// Lista modeli obsługiwanych przez ten backend.
    async fn models(&self) -> Vec<String>;

    /// Modele aktualnie załadowane w VRAM.
    async fn resident(&self) -> Vec<String>;

    /// Modele aktualnie załadowane wraz z ich rozmiarem w VRAM (MiB).
    async fn resident_with_vram(&self) -> Vec<BackendModelInfo> {
        self.resident()
            .await
            .into_iter()
            .map(|name| BackendModelInfo {
                name,
                size_vram_mib: 0,
            })
            .collect()
    }

    /// Szacowany rozmiar modelu w MiB (z `/api/tags` lub konfiguracji), używany przy budżecie VRAM.
    async fn estimated_vram_mib(&self, _model: &str) -> u64 {
        0
    }

    /// Przygotowanie backendu do obsługi modelu (np. rozgrzanie / sprawdzenie gotowości).
    async fn ensure_ready(&self, _model: &str) -> Result<(), String> {
        Ok(())
    }

    /// Zwolnienie VRAM (wyładowanie wszystkich załadowanych modeli).
    async fn unload_all(&self) -> Result<(), String>;

    /// Wyładowanie pojedynczego modelu z VRAM (domyślnie `unload_all`).
    async fn unload_model(&self, _model: &str) -> Result<(), String> {
        self.unload_all().await
    }

    /// Klasa rezydencji w VRAM.
    fn class(&self) -> ResidencyClass;

    /// Rodzaj backendu raportowany w `/status`.
    fn kind(&self) -> hub_proto::Backend {
        hub_proto::Backend::Ollama
    }
}

/// Implementacja backendu dla serwera Ollama.
pub struct OllamaBackend {
    name: String,
    base_url: String,
    client: reqwest::Client,
    static_models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaTagModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagModel {
    name: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Deserialize)]
struct OllamaPsResponse {
    #[serde(default)]
    models: Vec<OllamaPsModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaPsModel {
    name: String,
    #[serde(default)]
    size_vram: u64,
}

impl OllamaBackend {
    pub fn new(base_url: String, client: reqwest::Client) -> Self {
        Self {
            name: "ollama".to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
            static_models: Vec::new(),
        }
    }

    pub fn with_name_and_models(
        name: String,
        base_url: String,
        client: reqwest::Client,
        static_models: Vec<String>,
    ) -> Self {
        Self {
            name,
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
            static_models,
        }
    }

    async fn fetch_tags(&self) -> Option<Vec<OllamaTagModel>> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("Nie udało się pobrać /api/tags z Ollamy ({url}): {e}");
                return None;
            }
        };
        if !resp.status().is_success() {
            warn!("Ollama zwróciła HTTP {} dla {url}", resp.status());
            return None;
        }
        match resp.json::<OllamaTagsResponse>().await {
            Ok(parsed) => Some(parsed.models),
            Err(e) => {
                warn!("Błąd parsowania /api/tags z Ollamy: {e}");
                None
            }
        }
    }

    async fn fetch_ps(&self) -> Vec<OllamaPsModel> {
        let url = format!("{}/api/ps", self.base_url);
        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("Nie udało się pobrać /api/ps z Ollamy ({url}): {e}");
                return Vec::new();
            }
        };
        if !resp.status().is_success() {
            warn!("Ollama zwróciła HTTP {} dla {url}", resp.status());
            return Vec::new();
        }
        match resp.json::<OllamaPsResponse>().await {
            Ok(parsed) => parsed.models,
            Err(e) => {
                warn!("Błąd parsowania /api/ps z Ollamy: {e}");
                Vec::new()
            }
        }
    }

    async fn unload_one(&self, model: &str) -> Result<(), String> {
        let url = format!("{}/api/generate", self.base_url);
        let payload = json!({
            "model": model,
            "keep_alive": 0
        });
        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Błąd wyładowywania modelu {model} w Ollamie: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "Ollama zwróciła status {} przy wyładowywaniu modelu {model}",
                resp.status()
            ));
        }
        info!("Wyładowano model z Ollamy: {model}");
        Ok(())
    }
}

/// Sprawdza czy nazwa modelu pasuje (uwzględniając domyślny tag `:latest` w Ollamie).
pub fn model_names_match(registered: &str, requested: &str) -> bool {
    if registered == requested {
        return true;
    }
    if let Some(stripped) = registered.strip_suffix(":latest") {
        if stripped == requested {
            return true;
        }
    }
    if let Some(stripped) = requested.strip_suffix(":latest") {
        if stripped == registered {
            return true;
        }
    }
    false
}

#[async_trait]
impl Backend for OllamaBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn models(&self) -> Vec<String> {
        if let Some(tags) = self.fetch_tags().await {
            let mut names: Vec<String> = tags.into_iter().map(|m| m.name).collect();
            for s in &self.static_models {
                if !names.iter().any(|n| model_names_match(n, s)) {
                    names.push(s.clone());
                }
            }
            return names;
        }
        self.static_models.clone()
    }

    async fn resident(&self) -> Vec<String> {
        self.fetch_ps().await.into_iter().map(|m| m.name).collect()
    }

    async fn resident_with_vram(&self) -> Vec<BackendModelInfo> {
        self.fetch_ps()
            .await
            .into_iter()
            .map(|m| BackendModelInfo {
                name: m.name,
                size_vram_mib: m.size_vram / (1024 * 1024),
            })
            .collect()
    }

    async fn estimated_vram_mib(&self, model: &str) -> u64 {
        if let Some(tags) = self.fetch_tags().await {
            for m in tags {
                if model_names_match(&m.name, model) {
                    return m.size / (1024 * 1024);
                }
            }
        }
        0
    }

    async fn ensure_ready(&self, _model: &str) -> Result<(), String> {
        Ok(())
    }

    async fn unload_all(&self) -> Result<(), String> {
        let loaded = self.resident().await;
        for model in loaded {
            self.unload_one(&model).await?;
        }
        Ok(())
    }

    async fn unload_model(&self, model: &str) -> Result<(), String> {
        self.unload_one(model).await
    }

    fn class(&self) -> ResidencyClass {
        ResidencyClass::Shared
    }
}
