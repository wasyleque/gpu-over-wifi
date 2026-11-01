//! Backend InferSSD: zarządza procesem `inferssd serve` (wyłączność na GPU).
//!
//! InferSSD jest uruchamiany wyłącznie jako zewnętrzny proces; hub rozmawia z nim przez API OpenAI.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::backend::{model_names_match, Backend, ResidencyClass};

/// Parametry trybu `inferssd launch` (ustawienia, pack ekspertów, rozmiary partii).
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// Opis zasobów silnika (`--resources`); domyślnie z otoczenia opakowania `inferssd`.
    pub resources: Option<PathBuf>,
    /// Plik `inferssd-settings 1` (np. z `expert.source` wskazującym na pack).
    pub settings: Option<PathBuf>,
    pub context: Option<u32>,
    pub batch: Option<u32>,
    pub ubatch: Option<u32>,
    /// Limit pamięci procesu zapisywany do polityki uruchomienia.
    pub memory_budget_bytes: u64,
    /// Katalog na politykę i logi uruchomień; każdy start dostaje nowy podkatalog.
    pub run_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct InferssdConfig {
    pub name: String,
    pub binary: PathBuf,
    pub base_url: String,
    pub port: u16,
    /// alias widoczny dla klientów -> ścieżka do pierwszego shardu GGUF
    pub models: BTreeMap<String, PathBuf>,
    pub extra_args: Vec<String>,
    pub log_file: Option<PathBuf>,
    pub start_timeout: Duration,
    pub poll_interval: Duration,
    /// Po nieudanym starcie kolejne próby są odrzucane przez ten czas.
    pub retry_after: Duration,
    /// Czas na SIGTERM, zanim proces dostanie SIGKILL.
    pub term_grace: Duration,
    /// `Some` => `inferssd launch --execute ...`; `None` => `inferssd serve`.
    pub launch: Option<LaunchConfig>,
}

struct Running {
    child: Child,
    alias: String,
}

#[derive(Default)]
struct State {
    running: Option<Running>,
    failed: Option<(Instant, String)>,
}

pub struct InferssdBackend {
    cfg: InferssdConfig,
    client: reqwest::Client,
    state: Mutex<State>,
    /// Alias aktualnie gotowego modelu (odczytywany bez blokowania długiego startu).
    loaded: StdMutex<Option<String>>,
    /// Katalog ostatniego uruchomienia w trybie `launch` (zawiera `native-events.jsonl`).
    last_run: StdMutex<Option<PathBuf>>,
}

impl InferssdBackend {
    pub fn new(cfg: InferssdConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("Błąd inicjalizacji klienta HTTP InferSSD"),
            state: Mutex::new(State::default()),
            loaded: StdMutex::new(None),
            last_run: StdMutex::new(None),
        }
    }

    fn resolve_alias(&self, model: &str) -> Option<(&String, &PathBuf)> {
        self.cfg
            .models
            .iter()
            .find(|(alias, _)| model_names_match(alias, model))
    }

    /// Argumenty wiersza poleceń zależnie od trybu (`serve` albo `launch`).
    fn command_args(&self, path: &PathBuf) -> Result<Vec<std::ffi::OsString>, String> {
        use std::ffi::OsString;
        let mut a: Vec<OsString> = Vec::new();
        match &self.cfg.launch {
            None => {
                a.extend(["serve".into(), "-m".into(), path.into(), "--port".into()]);
                a.push(self.cfg.port.to_string().into());
            }
            Some(l) => {
                std::fs::create_dir_all(&l.run_dir)
                    .map_err(|e| format!("katalog uruchomień ({}): {e}", l.run_dir.display()))?;
                let policy_path = l.run_dir.join("runtime-policy.json");
                let policy = serde_json::json!({
                    "platform": "linux",
                    "lock_path": l.run_dir.join("runtime.lock"),
                    "stop_path": l.run_dir.join("runtime.stop"),
                    "memory_budget_bytes": l.memory_budget_bytes,
                    "minimum_memlock_bytes": 0,
                    "cleanup_seconds": 30,
                });
                std::fs::write(&policy_path, policy.to_string())
                    .map_err(|e| format!("polityka ({}): {e}", policy_path.display()))?;
                // `launch` wymaga NOWEGO katalogu wyjściowego przy każdym starcie.
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let out = l.run_dir.join(format!("run-{nanos}"));
                if let Ok(mut g) = self.last_run.lock() {
                    *g = Some(out.clone());
                }
                a.extend(["launch".into(), "--execute".into()]);
                if let Some(r) = &l.resources {
                    a.extend(["--resources".into(), r.into()]);
                }
                a.extend(["--model".into(), path.into()]);
                a.extend(["--runtime-policy".into(), policy_path.into()]);
                a.extend(["--output-dir".into(), out.into()]);
                a.extend(["--port".into(), self.cfg.port.to_string().into()]);
                for (flag, value) in [("--context", l.context), ("--batch", l.batch), ("--ubatch", l.ubatch)] {
                    if let Some(v) = value {
                        a.extend([flag.into(), v.to_string().into()]);
                    }
                }
                if let Some(st) = &l.settings {
                    a.extend(["--settings".into(), st.into()]);
                }
            }
        }
        a.extend(self.cfg.extra_args.iter().map(OsString::from));
        Ok(a)
    }

    fn spawn(&self, path: &PathBuf) -> Result<Child, String> {
        let (stdout, stderr) = match &self.cfg.log_file {
            Some(log) => {
                let open = || {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(log)
                        .map_err(|e| format!("log InferSSD ({}): {e}", log.display()))
                };
                (Stdio::from(open()?), Stdio::from(open()?))
            }
            None => (Stdio::null(), Stdio::null()),
        };
        let args = self.command_args(path)?;
        Command::new(&self.cfg.binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("start {}: {e}", self.cfg.binary.display()))
    }

    /// Czeka, aż serwer odpowie na `/v1/models`; przerywa, gdy proces zginie lub minie limit.
    async fn wait_ready(&self, child: &mut Child) -> Result<(), String> {
        let url = format!("{}/v1/models", self.cfg.base_url.trim_end_matches('/'));
        let deadline = Instant::now() + self.cfg.start_timeout;
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("proces InferSSD zakończył się podczas startu ({status})"));
            }
            if let Ok(resp) = self.client.get(&url).send().await {
                if resp.status().is_success() {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "InferSSD nie był gotowy po {} s",
                    self.cfg.start_timeout.as_secs()
                ));
            }
            tokio::time::sleep(self.cfg.poll_interval).await;
        }
    }

    /// Zatrzymuje całą grupę procesów: SIGTERM, a po `term_grace` SIGKILL.
    async fn terminate(&self, child: &mut Child) {
        let Some(pid) = child.id() else {
            let _ = child.wait().await;
            return;
        };
        let pgid = pid as i32;
        // SAFETY: wysyłamy sygnał do grupy procesów utworzonej przez `process_group(0)`.
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
        if tokio::time::timeout(self.cfg.term_grace, child.wait())
            .await
            .is_err()
        {
            warn!("InferSSD nie zakończył się po SIGTERM, wysyłam SIGKILL");
            // SAFETY: jak wyżej.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
            let _ = child.wait().await;
        }
    }

    async fn stop_locked(&self, state: &mut State) {
        if let Some(mut running) = state.running.take() {
            info!("Zatrzymuję InferSSD (model '{}')", running.alias);
            self.terminate(&mut running.child).await;
        }
        *self.loaded.lock().unwrap() = None;
    }
}

#[async_trait]
impl Backend for InferssdBackend {
    fn name(&self) -> &str {
        &self.cfg.name
    }

    fn base_url(&self) -> &str {
        &self.cfg.base_url
    }

    fn kind(&self) -> hub_proto::Backend {
        hub_proto::Backend::Inferssd
    }

    fn telemetry_path(&self) -> Option<PathBuf> {
        let dir = self.last_run.lock().ok()?.clone()?;
        Some(dir.join("native-events.jsonl"))
    }

    async fn models(&self) -> Vec<String> {
        self.cfg.models.keys().cloned().collect()
    }

    async fn resident(&self) -> Vec<String> {
        // Jeśli akurat nie trwa start/stop, sprawdź, czy proces nadal żyje.
        if let Ok(mut state) = self.state.try_lock() {
            let dead = match state.running.as_mut() {
                Some(r) => r.child.try_wait().ok().flatten().is_some(),
                None => false,
            };
            if dead {
                warn!("Proces InferSSD zakończył się niespodziewanie");
                state.running = None;
                *self.loaded.lock().unwrap() = None;
            }
        }
        self.loaded.lock().unwrap().iter().cloned().collect()
    }

    async fn ensure_ready(&self, model: &str) -> Result<(), String> {
        let (alias, path) = self
            .resolve_alias(model)
            .map(|(a, p)| (a.clone(), p.clone()))
            .ok_or_else(|| format!("InferSSD nie zna modelu '{model}'"))?;

        let mut state = self.state.lock().await;

        if let Some((when, msg)) = &state.failed {
            if when.elapsed() < self.cfg.retry_after {
                return Err(format!(
                    "poprzedni start InferSSD się nie udał ({msg}); kolejna próba za {} s",
                    (self.cfg.retry_after - when.elapsed()).as_secs()
                ));
            }
        }

        if let Some(r) = state.running.as_mut() {
            let alive = r.child.try_wait().map(|s| s.is_none()).unwrap_or(false);
            if alive && r.alias == alias {
                return Ok(());
            }
        }
        self.stop_locked(&mut state).await;

        info!("Uruchamiam InferSSD z modelem '{alias}' ({})", path.display());
        let mut child = match self.spawn(&path) {
            Ok(c) => c,
            Err(e) => {
                state.failed = Some((Instant::now(), e.clone()));
                return Err(e);
            }
        };
        match self.wait_ready(&mut child).await {
            Ok(()) => {
                state.failed = None;
                state.running = Some(Running {
                    child,
                    alias: alias.clone(),
                });
                *self.loaded.lock().unwrap() = Some(alias);
                Ok(())
            }
            Err(e) => {
                self.terminate(&mut child).await;
                state.failed = Some((Instant::now(), e.clone()));
                Err(e)
            }
        }
    }

    async fn unload_all(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        self.stop_locked(&mut state).await;
        Ok(())
    }

    fn class(&self) -> ResidencyClass {
        ResidencyClass::Exclusive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn port_open(port: u16) -> bool {
        std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_ok()
    }

    /// Atrapa `inferssd`: po `delay` s wystawia `/v1/models`. `prologue` wstawiany przed startem.
    fn fake_binary(dir: &std::path::Path, delay: f32, prologue: &str) -> PathBuf {
        let path = dir.join("fake-inferssd");
        let script = format!(
            r#"#!/bin/sh
{prologue}
PORT=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--port" ]; then PORT="$2"; fi
  shift
done
sleep {delay}
exec python3 -u -c '
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        b = b"{{\"data\":[{{\"id\":\"fake\"}}]}}"
        self.send_response(200)
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
' "$PORT"
"#
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hub-inferssd-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn backend(binary: PathBuf, port: u16) -> InferssdBackend {
        let mut models = BTreeMap::new();
        models.insert("deepseek".to_string(), PathBuf::from("/tmp/fake-first-shard.gguf"));
        InferssdBackend::new(InferssdConfig {
            name: "inferssd".into(),
            binary,
            base_url: format!("http://127.0.0.1:{port}"),
            port,
            models,
            extra_args: vec![],
            log_file: None,
            start_timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(50),
            retry_after: Duration::from_secs(60),
            term_grace: Duration::from_millis(300),
            launch: None,
        })
    }

    #[tokio::test]
    async fn starts_waits_for_readiness_and_stops() {
        let dir = temp_dir("start");
        let port = free_port();
        let b = backend(fake_binary(&dir, 0.4, ""), port);

        assert!(b.resident().await.is_empty());
        let started = Instant::now();
        b.ensure_ready("deepseek").await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300), "powinien czekać na gotowość");
        assert_eq!(b.resident().await, vec!["deepseek".to_string()]);
        assert!(port_open(port));

        // Drugie wywołanie dla tego samego modelu nie restartuje procesu.
        let again = Instant::now();
        b.ensure_ready("deepseek").await.unwrap();
        assert!(again.elapsed() < Duration::from_millis(200));

        b.unload_all().await.unwrap();
        assert!(b.resident().await.is_empty());
        assert!(!port_open(port), "port powinien być zamknięty po zatrzymaniu");
    }

    #[tokio::test]
    async fn unknown_alias_is_an_error() {
        let dir = temp_dir("alias");
        let b = backend(fake_binary(&dir, 0.0, ""), free_port());
        assert!(b.ensure_ready("nie-ma").await.is_err());
        assert_eq!(b.models().await, vec!["deepseek".to_string()]);
    }

    #[tokio::test]
    async fn failed_start_is_reported_and_not_retried_in_a_loop() {
        let dir = temp_dir("fail");
        let b = backend(fake_binary(&dir, 0.0, "exit 3"), free_port());

        let err = b.ensure_ready("deepseek").await.unwrap_err();
        assert!(err.contains("zakończył się"), "{err}");
        assert!(b.resident().await.is_empty());

        // Kolejna próba w oknie `retry_after` jest odrzucana od razu, bez uruchamiania procesu.
        let t = Instant::now();
        let err2 = b.ensure_ready("deepseek").await.unwrap_err();
        assert!(err2.contains("kolejna próba"), "{err2}");
        assert!(t.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn sigterm_ignoring_process_is_killed() {
        let dir = temp_dir("kill");
        let port = free_port();
        let b = backend(fake_binary(&dir, 0.2, "trap '' TERM"), port);
        b.ensure_ready("deepseek").await.unwrap();
        assert!(port_open(port));

        b.unload_all().await.unwrap();
        assert!(!port_open(port), "SIGKILL powinien zakończyć proces ignorujący SIGTERM");
    }

    #[tokio::test]
    async fn launch_mode_passes_expected_arguments_and_fresh_output_dirs() {
        let dir = temp_dir("launch");
        let run_dir = dir.join("runs");
        let port = free_port();
        // Atrapa zapisuje otrzymane argumenty (jedna linia na start).
        let bin = fake_binary(&dir, 0.1, &format!("echo \"$@\" >> {}/args.log", dir.display()));
        let mut b = backend(bin, port);
        b.cfg.launch = Some(LaunchConfig {
            resources: Some(PathBuf::from("/opt/res.json")),
            settings: Some(PathBuf::from("/opt/pack.ssd")),
            context: Some(16384),
            batch: Some(2048),
            ubatch: Some(2048),
            memory_budget_bytes: 123_456_789,
            run_dir: run_dir.clone(),
        });

        b.ensure_ready("deepseek").await.unwrap();
        b.unload_all().await.unwrap();
        b.ensure_ready("deepseek").await.unwrap();
        b.unload_all().await.unwrap();

        let log = std::fs::read_to_string(dir.join("args.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "dwa starty => dwie linie: {log}");
        for l in &lines {
            assert!(l.starts_with("launch --execute --resources /opt/res.json --model /tmp/fake-first-shard.gguf --runtime-policy "), "{l}");
            assert!(l.contains(&format!("--port {port} --context 16384 --batch 2048 --ubatch 2048 --settings /opt/pack.ssd")), "{l}");
        }
        let outs: Vec<&str> = lines.iter().map(|l| l.split("--output-dir ").nth(1).unwrap().split(' ').next().unwrap()).collect();
        assert_ne!(outs[0], outs[1], "każdy start potrzebuje nowego katalogu wyjściowego");
        assert!(outs[0].starts_with(run_dir.to_str().unwrap()));
        let policy: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(run_dir.join("runtime-policy.json")).unwrap()).unwrap();
        assert_eq!(policy["memory_budget_bytes"], 123_456_789u64);
        assert_eq!(policy["platform"], "linux");
    }

    #[tokio::test]
    async fn serve_mode_arguments_are_unchanged() {
        let dir = temp_dir("serveargs");
        let port = free_port();
        let bin = fake_binary(&dir, 0.1, &format!("echo \"$@\" >> {}/args.log", dir.display()));
        let b = backend(bin, port);
        b.ensure_ready("deepseek").await.unwrap();
        b.unload_all().await.unwrap();
        let log = std::fs::read_to_string(dir.join("args.log")).unwrap();
        assert_eq!(log.trim(), format!("serve -m /tmp/fake-first-shard.gguf --port {port}"));
    }
}
