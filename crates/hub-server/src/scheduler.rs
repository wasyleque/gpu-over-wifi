use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hub_proto::SchedulerStatus;
use serde::Deserialize;
use tokio::sync::{oneshot, Mutex, Notify};
use tracing::{info, warn};

use crate::backend::{model_names_match, Backend, ResidencyClass};

pub const DEFAULT_MAX_WAIT_SECS: u64 = 120;
pub const DEFAULT_MAX_CONCURRENT_PER_MODEL: usize = 4;
pub const DEFAULT_VRAM_BUDGET_MIB: u64 = 22_528;

/// Zegar używany przez scheduler (umożliwia wstrzykiwanie czasu w testach).
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Konfiguracja z pliku `hub.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HubConfig {
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default, rename = "backend")]
    pub backends: Vec<BackendConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchedulerConfig {
    #[serde(default = "default_max_wait_secs")]
    pub max_wait_secs: u64,
    #[serde(default = "default_max_concurrent_per_model")]
    pub max_concurrent_per_model: usize,
    #[serde(default = "default_vram_budget_mib")]
    pub vram_budget_mib: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_wait_secs: DEFAULT_MAX_WAIT_SECS,
            max_concurrent_per_model: DEFAULT_MAX_CONCURRENT_PER_MODEL,
            vram_budget_mib: DEFAULT_VRAM_BUDGET_MIB,
        }
    }
}

fn default_max_wait_secs() -> u64 {
    DEFAULT_MAX_WAIT_SECS
}

fn default_max_concurrent_per_model() -> usize {
    DEFAULT_MAX_CONCURRENT_PER_MODEL
}

fn default_vram_budget_mib() -> u64 {
    DEFAULT_VRAM_BUDGET_MIB
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackendConfig {
    pub name: String,
    #[serde(default = "default_backend_kind")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub models: Vec<String>,
}

fn default_backend_kind() -> String {
    "ollama".to_string()
}

impl HubConfig {
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "Nie udało się odczytać pliku konfiguracyjnego ({}): {e}",
                path.display()
            )
        })?;
        toml::from_str(&content).map_err(|e| {
            format!(
                "Błąd parsowania pliku konfiguracyjnego ({}): {e}",
                path.display()
            )
        })
    }
}

#[derive(Debug)]
pub enum ScheduleError {
    UnknownModel(String),
    BackendError(String),
    Cancelled,
}

struct QueuedItem {
    id: u64,
    model: String,
    backend_idx: usize,
    enqueued_at: Instant,
    tx: oneshot::Sender<LeaseToken>,
}

struct LeaseToken {
    backend_idx: usize,
    model: String,
}

struct InnerState {
    queue: Vec<QueuedItem>,
    in_flight_per_model: HashMap<String, usize>,
    in_flight_per_backend: HashMap<usize, usize>,
    total_in_flight: u32,
    active_backend: Option<usize>,
    known_resident: HashSet<String>,
    switches_total: u64,
    switching: bool,
}

pub struct Scheduler {
    config: SchedulerConfig,
    backends: Vec<Arc<dyn Backend>>,
    clock: Arc<dyn Clock>,
    inner: Mutex<InnerState>,
    notify: Notify,
    next_id: AtomicU64,
}

/// Strażnik aktywnego żądania; zwalnia slot współbieżności po zakończeniu (w tym po zamknięciu strumienia).
pub struct LeaseGuard {
    scheduler: Arc<Scheduler>,
    backend_idx: usize,
    model: String,
    base_url: String,
}

impl LeaseGuard {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let scheduler = self.scheduler.clone();
        let backend_idx = self.backend_idx;
        let model = self.model.clone();
        let released = match scheduler.inner.try_lock() {
            Ok(mut guard) => {
                Self::release_locked(&mut guard, backend_idx, &model);
                true
            }
            Err(_) => false,
        };
        if released {
            scheduler.notify.notify_waiters();
            scheduler.spawn_dispatch();
        } else {
            tokio::spawn(async move {
                {
                    let mut guard = scheduler.inner.lock().await;
                    Self::release_locked(&mut guard, backend_idx, &model);
                }
                scheduler.notify.notify_waiters();
                scheduler.dispatch_loop().await;
            });
        }
    }
}

impl LeaseGuard {
    fn release_locked(inner: &mut InnerState, backend_idx: usize, model: &str) {
        if let Some(c) = inner.in_flight_per_model.get_mut(model) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                inner.in_flight_per_model.remove(model);
            }
        }
        if let Some(c) = inner.in_flight_per_backend.get_mut(&backend_idx) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                inner.in_flight_per_backend.remove(&backend_idx);
            }
        }
        inner.total_in_flight = inner.total_in_flight.saturating_sub(1);
    }
}

/// Strażnik usuwający żądanie z kolejki w przypadku anulowania (rozłączenia klienta) przed przydzieleniem slotu.
struct QueueCancelGuard {
    scheduler: Arc<Scheduler>,
    id: u64,
    disarmed: bool,
}

impl Drop for QueueCancelGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let scheduler = self.scheduler.clone();
        let id = self.id;
        let removed = match scheduler.inner.try_lock() {
            Ok(mut guard) => {
                guard.queue.retain(|item| item.id != id);
                true
            }
            Err(_) => false,
        };
        if removed {
            scheduler.notify.notify_waiters();
        } else {
            tokio::spawn(async move {
                {
                    let mut guard = scheduler.inner.lock().await;
                    guard.queue.retain(|item| item.id != id);
                }
                scheduler.notify.notify_waiters();
            });
        }
    }
}

impl Scheduler {
    pub fn new(config: SchedulerConfig, backends: Vec<Arc<dyn Backend>>) -> Arc<Self> {
        Self::with_clock(config, backends, Arc::new(SystemClock))
    }

    pub fn with_clock(
        config: SchedulerConfig,
        backends: Vec<Arc<dyn Backend>>,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            backends,
            clock,
            inner: Mutex::new(InnerState {
                queue: Vec::new(),
                in_flight_per_model: HashMap::new(),
                in_flight_per_backend: HashMap::new(),
                total_in_flight: 0,
                active_backend: None,
                known_resident: HashSet::new(),
                switches_total: 0,
                switching: false,
            }),
            notify: Notify::new(),
            next_id: AtomicU64::new(1),
        })
    }

    /// Zwraca URL domyślnego backendu (pierwszy zarejestrowany, czyli Ollama).
    pub fn default_backend_url(&self) -> String {
        self.backends
            .first()
            .map(|b| b.base_url().to_string())
            .unwrap_or_else(|| "http://127.0.0.1:11434".to_string())
    }

    /// Zwraca bieżący stan obserwowalny schedulera dla `GET /status`.
    pub async fn status(&self) -> SchedulerStatus {
        let mut inner = self.inner.lock().await;
        inner.queue.retain(|item| !item.tx.is_closed());
        let active_backend = inner
            .active_backend
            .and_then(|idx| self.backends.get(idx))
            .map(|b| b.name().to_string())
            .or_else(|| self.backends.first().map(|b| b.name().to_string()));

        SchedulerStatus {
            queue_len: inner.queue.len() as u32,
            active_backend,
            switches_total: inner.switches_total,
            in_flight: inner.total_in_flight,
        }
    }

    /// Powiadamia scheduler o upływie czasu (przydatne w testach z mockowanym zegarem).
    #[allow(dead_code)]
    pub async fn tick(self: &Arc<Self>) {
        self.dispatch_loop().await;
    }

    /// Rozwiązuje model na indeks backendu oraz kanoniczną nazwę modelu.
    async fn resolve_model(&self, requested_model: &str) -> Option<(usize, String)> {
        let mut any_models_reported = false;
        for (idx, backend) in self.backends.iter().enumerate() {
            let models = backend.models().await;
            if !models.is_empty() {
                any_models_reported = true;
            }
            for m in models {
                if model_names_match(&m, requested_model) {
                    return Some((idx, requested_model.to_string()));
                }
            }
        }
        // Fallback: jeśli żaden backend nie zwrócił jeszcze listy modeli (np. w prostych testach proxy bez /api/tags),
        // tylko wtedy, gdy jedyny backend nie zgłosił żadnych modeli?
        // UWAGA: specyfikacja mówi "Nieznany model -> 404 JSON {"error":"unknown model"}".
        let _ = any_models_reported;
        None
    }

    /// Kolejkuje żądanie do konkretnego modelu i zwraca `LeaseGuard` z docelowym `base_url`.
    pub async fn acquire(self: &Arc<Self>, requested_model: &str) -> Result<LeaseGuard, ScheduleError> {
        let Some((backend_idx, model)) = self.resolve_model(requested_model).await else {
            return Err(ScheduleError::UnknownModel(requested_model.to_string()));
        };

        // Odśwież informację o załadowanych modelach, jeśli brak aktywnych zadań
        self.refresh_residency_if_idle(backend_idx).await;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let now = self.clock.now();

        {
            let mut inner = self.inner.lock().await;
            inner.queue.push(QueuedItem {
                id,
                model: model.clone(),
                backend_idx,
                enqueued_at: now,
                tx,
            });
        }

        let mut cancel_guard = QueueCancelGuard {
            scheduler: self.clone(),
            id,
            disarmed: false,
        };

        // Dispatch musi działać poza zadaniem żądającego: przydział dla tego żądania i kolejne
        // przełączenie (z oczekiwaniem na drain) w jednej pętli dawałyby zakleszczenie.
        self.spawn_dispatch();

        match rx.await {
            Ok(token) => {
                cancel_guard.disarmed = true;
                let base_url = self.backends[token.backend_idx].base_url().to_string();
                Ok(LeaseGuard {
                    scheduler: self.clone(),
                    backend_idx: token.backend_idx,
                    model: token.model,
                    base_url,
                })
            }
            Err(_) => Err(ScheduleError::Cancelled),
        }
    }

    async fn refresh_residency_if_idle(&self, backend_idx: usize) {
        let is_empty = {
            let inner = self.inner.lock().await;
            inner.known_resident.is_empty() && inner.total_in_flight == 0 && !inner.switching
        };
        if is_empty {
            if let Some(backend) = self.backends.get(backend_idx) {
                let resident = backend.resident().await;
                if !resident.is_empty() {
                    let mut inner = self.inner.lock().await;
                    if inner.known_resident.is_empty() {
                        for r in resident {
                            inner.known_resident.insert(r);
                        }
                        if inner.active_backend.is_none() {
                            inner.active_backend = Some(backend_idx);
                        }
                    }
                }
            }
        }
    }

    fn spawn_dispatch(self: &Arc<Self>) {
        let sched = self.clone();
        tokio::spawn(async move {
            sched.dispatch_loop().await;
        });
    }

    pub async fn dispatch_loop(self: &Arc<Self>) {
        loop {
            enum Action {
                None,
                FastDispatch(QueuedItem),
                NeedSwitch {
                    target_backend_idx: usize,
                    target_model: String,
                },
            }

            let action = {
                let mut inner = self.inner.lock().await;
                inner.queue.retain(|item| !item.tx.is_closed());

                if inner.switching || inner.queue.is_empty() {
                    Action::None
                } else {
                    let now = self.clock.now();
                    let max_wait = Duration::from_secs(self.config.max_wait_secs);

                    let is_model_hot = |m: &str, b_idx: usize, st: &InnerState| -> bool {
                        let backend_matches =
                            st.active_backend.is_none() || st.active_backend == Some(b_idx);
                        backend_matches
                            && (st
                                .known_resident
                                .iter()
                                .any(|r| model_names_match(r, m))
                                || st
                                    .in_flight_per_model
                                    .keys()
                                    .any(|k| model_names_match(k, m)))
                    };

                    // 1. Sprawdź czy w kolejce jest przeterminowane żądanie (aging > max_wait_secs)
                    let aged_idx = inner
                        .queue
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| now.saturating_duration_since(item.enqueued_at) >= max_wait)
                        .min_by_key(|(_, item)| item.enqueued_at)
                        .map(|(idx, _)| idx);

                    if let Some(idx) = aged_idx {
                        let candidate_model = inner.queue[idx].model.clone();
                        let candidate_backend = inner.queue[idx].backend_idx;
                        let hot = is_model_hot(&candidate_model, candidate_backend, &inner);
                        let in_flight_m = inner
                            .in_flight_per_model
                            .get(&candidate_model)
                            .copied()
                            .unwrap_or(0);

                        if hot {
                            if in_flight_m < self.config.max_concurrent_per_model {
                                let item = inner.queue.remove(idx);
                                Action::FastDispatch(item)
                            } else {
                                Action::None
                            }
                        } else {
                            // Przeterminowane żądanie wymaga przełączenia modelu/backendu — blokujemy nowe żądania do innych modeli
                            inner.switching = true;
                            Action::NeedSwitch {
                                target_backend_idx: candidate_backend,
                                target_model: candidate_model,
                            }
                        }
                    } else {
                        // 2. Grupowanie po modelu: szukamy pierwszego żądania do modelu już załadowanego (hot),
                        // który ma wolny slot współbieżności.
                        let hot_ready_idx = inner.queue.iter().position(|item| {
                            if !is_model_hot(&item.model, item.backend_idx, &inner) {
                                return false;
                            }
                            let in_flight_m = inner
                                .in_flight_per_model
                                .get(&item.model)
                                .copied()
                                .unwrap_or(0);
                            in_flight_m < self.config.max_concurrent_per_model
                        });

                        if let Some(idx) = hot_ready_idx {
                            let item = inner.queue.remove(idx);
                            Action::FastDispatch(item)
                        } else {
                            // Sprawdź czy są jakiekolwiek żądania do gorącego modelu, które czekają tylko na zwolnienie slotu współbieżności
                            let any_hot_waiting = inner
                                .queue
                                .iter()
                                .any(|item| is_model_hot(&item.model, item.backend_idx, &inner));

                            if any_hot_waiting {
                                // Czekamy aż gorący model zwolni slot, nie przełączamy jeszcze na zimny model
                                Action::None
                            } else {
                                // Brak żądań do gorących modeli — wybieramy najstarsze żądanie z kolejki
                                let candidate_model = inner.queue[0].model.clone();
                                let candidate_backend = inner.queue[0].backend_idx;
                                inner.switching = true;
                                Action::NeedSwitch {
                                    target_backend_idx: candidate_backend,
                                    target_model: candidate_model,
                                }
                            }
                        }
                    }
                }
            };

            match action {
                Action::None => break,
                Action::FastDispatch(item) => {
                    self.grant_item(item).await;
                }
                Action::NeedSwitch {
                    target_backend_idx,
                    target_model,
                } => {
                    self.perform_switch_and_grant(target_backend_idx, &target_model)
                        .await;
                }
            }
        }
    }

    async fn grant_item(&self, item: QueuedItem) {
        let mut inner = self.inner.lock().await;
        if item.tx.is_closed() {
            return;
        }
        let backend_idx = item.backend_idx;
        let model = item.model.clone();
        if item
            .tx
            .send(LeaseToken {
                backend_idx,
                model: model.clone(),
            })
            .is_ok()
        {
            *inner.in_flight_per_model.entry(model.clone()).or_insert(0) += 1;
            *inner.in_flight_per_backend.entry(backend_idx).or_insert(0) += 1;
            inner.total_in_flight += 1;
            inner.active_backend = Some(backend_idx);
            inner.known_resident.insert(model);
        }
    }

    async fn perform_switch_and_grant(&self, target_backend_idx: usize, target_model: &str) {
        let target_backend = &self.backends[target_backend_idx];

        // Sprawdź czy zmieniamy backend lub czy model mieści się w budżecie VRAM
        let prev_backend_idx = {
            let inner = self.inner.lock().await;
            inner.active_backend
        };

        let backend_changed = prev_backend_idx.is_some() && prev_backend_idx != Some(target_backend_idx);

        if backend_changed {
            // Drain poprzedniego backendu
            self.wait_for_drain(|inner| inner.total_in_flight == 0).await;
            if let Some(prev_idx) = prev_backend_idx {
                if let Err(e) = self.backends[prev_idx].unload_all().await {
                    warn!("Błąd unload_all na backendzie {}: {e}", self.backends[prev_idx].name());
                }
            }
            let mut inner = self.inner.lock().await;
            inner.known_resident.clear();
            inner.switches_total += 1;
        } else {
            match target_backend.class() {
                ResidencyClass::Exclusive => {
                    let need_switch = {
                        let inner = self.inner.lock().await;
                        !inner.known_resident.is_empty()
                            && !inner
                                .known_resident
                                .iter()
                                .any(|r| model_names_match(r, target_model))
                    };
                    self.wait_for_drain(|inner| inner.total_in_flight == 0).await;
                    if need_switch {
                        if let Err(e) = target_backend.unload_all().await {
                            warn!("Błąd unload_all na backendzie {}: {e}", target_backend.name());
                        }
                        let mut inner = self.inner.lock().await;
                        inner.known_resident.clear();
                        inner.switches_total += 1;
                    }
                }
                ResidencyClass::Shared => {
                    let resident_infos = target_backend.resident_with_vram().await;
                    let already_resident = resident_infos
                        .iter()
                        .any(|r| model_names_match(&r.name, target_model));

                    let current_vram: u64 = resident_infos.iter().map(|r| r.size_vram_mib).sum();
                    let estimated_new = target_backend.estimated_vram_mib(target_model).await;

                    let has_prior_models = {
                        let inner = self.inner.lock().await;
                        !inner.known_resident.is_empty() || !resident_infos.is_empty()
                    };

                    // Przełączenie zachodzi gdy nowy model nie mieści się obok załadowanych
                    // albo gdy mamy aktywne inne modele a budżet jest przekroczony.
                    let exceeds_budget = current_vram.saturating_add(estimated_new)
                        > self.config.vram_budget_mib
                        || (current_vram == 0 && estimated_new == 0 && has_prior_models && !already_resident);

                    if !already_resident && exceeds_budget {
                        // Drain aktywnych żądań przed wyładowaniem wypartych modeli
                        self.wait_for_drain(|inner| inner.total_in_flight == 0).await;
                        if let Err(e) = target_backend.unload_all().await {
                            warn!("Błąd unload_all w {}: {e}", target_backend.name());
                        }
                        let mut inner = self.inner.lock().await;
                        inner.known_resident.clear();
                        inner.switches_total += 1;
                    } else if !already_resident && has_prior_models {
                        // Model mieści się obok, ale odnotowujemy załadowanie
                        let mut inner = self.inner.lock().await;
                        inner.known_resident.insert(target_model.to_string());
                    }
                }
            }
        }

        if let Err(e) = target_backend.ensure_ready(target_model).await {
            warn!("ensure_ready nie powiodło się dla {target_model}: {e}");
        }

        // Po przygotowaniu modelu wpuść z kolejki wszystkie gotowe żądania do tego modelu (do limitu współbieżności)
        {
            let mut inner = self.inner.lock().await;
            inner.queue.retain(|item| !item.tx.is_closed());
            inner.active_backend = Some(target_backend_idx);
            inner.known_resident.insert(target_model.to_string());

            let mut i = 0;
            while i < inner.queue.len() {
                let in_flight_m = inner
                    .in_flight_per_model
                    .get(target_model)
                    .copied()
                    .unwrap_or(0);
                if in_flight_m >= self.config.max_concurrent_per_model {
                    break;
                }
                if inner.queue[i].backend_idx == target_backend_idx
                    && model_names_match(&inner.queue[i].model, target_model)
                {
                    let item = inner.queue.remove(i);
                    let m = item.model.clone();
                    if item
                        .tx
                        .send(LeaseToken {
                            backend_idx: target_backend_idx,
                            model: m.clone(),
                        })
                        .is_ok()
                    {
                        *inner.in_flight_per_model.entry(m).or_insert(0) += 1;
                        *inner
                            .in_flight_per_backend
                            .entry(target_backend_idx)
                            .or_insert(0) += 1;
                        inner.total_in_flight += 1;
                    }
                } else {
                    i += 1;
                }
            }

            inner.switching = false;
            info!(
                "Aktywowano model '{}' na backendzie '{}' (switches_total={})",
                target_model,
                target_backend.name(),
                inner.switches_total
            );
        }
    }

    async fn wait_for_drain<F>(&self, cond: F)
    where
        F: Fn(&InnerState) -> bool,
    {
        loop {
            let notified = self.notify.notified();
            {
                let inner = self.inner.lock().await;
                if cond(&inner) {
                    return;
                }
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;
    use tokio::task::JoinHandle;

    /// Atrapa backendu współdzielonego: pamięta załadowane modele i liczy wywołania `unload_all`.
    struct MockBackend {
        models: Vec<String>,
        resident: StdMutex<Vec<String>>,
        unloads: AtomicUsize,
    }

    impl MockBackend {
        fn new(models: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                models: models.iter().map(|m| m.to_string()).collect(),
                resident: StdMutex::new(Vec::new()),
                unloads: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl Backend for MockBackend {
        fn name(&self) -> &str {
            "mock"
        }
        fn base_url(&self) -> &str {
            "http://mock"
        }
        async fn models(&self) -> Vec<String> {
            self.models.clone()
        }
        async fn resident(&self) -> Vec<String> {
            self.resident.lock().unwrap().clone()
        }
        async fn ensure_ready(&self, model: &str) -> Result<(), String> {
            let mut r = self.resident.lock().unwrap();
            if !r.iter().any(|m| m == model) {
                r.push(model.to_string());
            }
            Ok(())
        }
        async fn unload_all(&self) -> Result<(), String> {
            self.unloads.fetch_add(1, Ordering::SeqCst);
            self.resident.lock().unwrap().clear();
            Ok(())
        }
        fn class(&self) -> ResidencyClass {
            ResidencyClass::Shared
        }
    }

    /// Zegar sterowany ręcznie.
    struct ManualClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl ManualClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            })
        }
        fn advance(&self, secs: u64) {
            self.offset_ms.fetch_add(secs * 1000, Ordering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
        }
    }

    fn config(max_concurrent: usize) -> SchedulerConfig {
        SchedulerConfig {
            max_wait_secs: 120,
            max_concurrent_per_model: max_concurrent,
            vram_budget_mib: DEFAULT_VRAM_BUDGET_MIB,
        }
    }

    /// Czeka (z limitem), aż kolejka osiągnie zadaną długość.
    async fn wait_queue_len(s: &Arc<Scheduler>, len: u32) {
        for _ in 0..200 {
            if s.status().await.queue_len == len {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("kolejka nie osiągnęła długości {len}");
    }

    /// Zadanie: pobierz dzierżawę, zapisz kolejność przydziału i od razu ją zwolnij.
    fn spawn_request(
        s: &Arc<Scheduler>,
        model: &'static str,
        tag: &'static str,
        order: &Arc<StdMutex<Vec<&'static str>>>,
    ) -> JoinHandle<()> {
        let s = s.clone();
        let order = order.clone();
        tokio::spawn(async move {
            let lease = s.acquire(model).await.expect("acquire");
            order.lock().unwrap().push(tag);
            drop(lease);
        })
    }

    async fn join_all(handles: Vec<JoinHandle<()>>) {
        for h in handles {
            tokio::time::timeout(Duration::from_secs(5), h)
                .await
                .expect("zadanie nie zakończyło się w czasie")
                .unwrap();
        }
    }

    #[tokio::test]
    async fn unknown_model_is_rejected() {
        let s = Scheduler::new(config(1), vec![MockBackend::new(&["a"])]);
        assert!(matches!(
            s.acquire("nie-ma-takiego").await,
            Err(ScheduleError::UnknownModel(_))
        ));
    }

    #[tokio::test]
    async fn groups_requests_by_model_to_minimise_switches() {
        let backend = MockBackend::new(&["a", "b"]);
        let s = Scheduler::new(config(1), vec![backend.clone()]);
        let order = Arc::new(StdMutex::new(Vec::new()));

        // A1 trzyma slot, więc reszta ustawia się w kolejce w kolejności FIFO: A2, B1, A3.
        let held = s.acquire("a").await.unwrap();
        let mut handles = vec![spawn_request(&s, "a", "A2", &order)];
        wait_queue_len(&s, 1).await;
        handles.push(spawn_request(&s, "b", "B1", &order));
        wait_queue_len(&s, 2).await;
        handles.push(spawn_request(&s, "a", "A3", &order));
        wait_queue_len(&s, 3).await;

        drop(held);
        join_all(handles).await;

        assert_eq!(*order.lock().unwrap(), vec!["A2", "A3", "B1"]);
        let status = s.status().await;
        assert_eq!(status.switches_total, 1, "jedno przełączenie a→b");
        assert_eq!(backend.unloads.load(Ordering::SeqCst), 1);
        assert_eq!(status.queue_len, 0);
        assert_eq!(status.in_flight, 0);
    }

    #[tokio::test]
    async fn aged_request_overtakes_hot_model() {
        let clock = ManualClock::new();
        let s = Scheduler::with_clock(config(1), vec![MockBackend::new(&["a", "b"])], clock.clone());
        let order = Arc::new(StdMutex::new(Vec::new()));

        // A1 trzyma slot. A2 i B1 trafiają do kolejki w tej samej chwili (B1 nie przełącza,
        // bo A2 czeka na gorący model).
        let held = s.acquire("a").await.unwrap();
        let mut handles = vec![spawn_request(&s, "a", "A2", &order)];
        wait_queue_len(&s, 1).await;
        handles.push(spawn_request(&s, "b", "B1", &order));
        wait_queue_len(&s, 2).await;

        // A3 dochodzi 100 s później; po kolejnych 25 s B1 ma 125 s (> 120), A3 tylko 25 s.
        clock.advance(100);
        handles.push(spawn_request(&s, "a", "A3", &order));
        wait_queue_len(&s, 3).await;
        clock.advance(25);

        drop(held);
        join_all(handles).await;

        // Samo grupowanie dałoby A2, A3, B1; aging wymusza B1 przed świeżym A3.
        assert_eq!(*order.lock().unwrap(), vec!["A2", "B1", "A3"]);
    }

    #[tokio::test]
    async fn cold_requests_for_several_models_do_not_deadlock() {
        let s = Scheduler::new(config(1), vec![MockBackend::new(&["a", "b", "c"])]);
        let order = Arc::new(StdMutex::new(Vec::new()));

        // Dwa zimne modele czekają na zwolnienie gorącego A; scheduler musi obsłużyć oba.
        let held = s.acquire("a").await.unwrap();
        let mut handles = vec![spawn_request(&s, "b", "B1", &order)];
        wait_queue_len(&s, 1).await;
        handles.push(spawn_request(&s, "c", "C1", &order));
        wait_queue_len(&s, 2).await;

        drop(held);
        join_all(handles).await;

        let mut got = order.lock().unwrap().clone();
        got.sort_unstable();
        assert_eq!(got, vec!["B1", "C1"]);
        let status = s.status().await;
        assert_eq!(status.queue_len, 0);
        assert_eq!(status.in_flight, 0);
    }

    #[tokio::test]
    async fn respects_max_concurrent_per_model() {
        let s = Scheduler::new(config(2), vec![MockBackend::new(&["a"])]);
        let first = s.acquire("a").await.unwrap();
        let second = s.acquire("a").await.unwrap();
        assert_eq!(s.status().await.in_flight, 2);

        // Trzecie żądanie nie może dostać slotu, dopóki któryś nie zostanie zwolniony.
        let s2 = s.clone();
        let third = tokio::spawn(async move { s2.acquire("a").await.map(|_| ()) });
        wait_queue_len(&s, 1).await;
        assert!(!third.is_finished());
        assert_eq!(s.status().await.in_flight, 2);

        drop(first);
        tokio::time::timeout(Duration::from_secs(5), third)
            .await
            .expect("trzecie żądanie powinno dostać slot")
            .unwrap()
            .unwrap();
        drop(second);
    }

    #[tokio::test]
    async fn cancelled_request_leaves_the_queue() {
        let s = Scheduler::new(config(1), vec![MockBackend::new(&["a"])]);
        let held = s.acquire("a").await.unwrap();

        let s2 = s.clone();
        let waiting = tokio::spawn(async move { s2.acquire("a").await.map(|_| ()) });
        wait_queue_len(&s, 1).await;

        waiting.abort();
        let _ = waiting.await;
        wait_queue_len(&s, 0).await;

        // Slot zwolniony przez pierwszego nie trafia do porzuconego żądania.
        drop(held);
        let next = tokio::time::timeout(Duration::from_secs(5), s.acquire("a"))
            .await
            .expect("kolejne żądanie powinno przejść od razu");
        assert!(next.is_ok());
        assert_eq!(s.status().await.queue_len, 0);
    }
}
