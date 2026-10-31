use clap::{Parser, Subcommand, ValueEnum};
use hub_proto::{Backend, StatusResponse, PROTOCOL_VERSION};

#[derive(Parser)]
#[command(name = "hubctl", about = "GPU Hub CLI client")]
struct Cli {
    /// Server URL
    #[arg(long, default_value = "http://127.0.0.1:7878", env = "HUB_SERVER")]
    server: String,

    /// Authorization token
    #[arg(long, env = "HUB_TOKEN")]
    token: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show hub status (GPU, loaded models, scheduler)
    Status,
    /// List models available through the hub
    Models,
    /// Print environment variables that point OpenAI-compatible tools at the hub
    ///
    /// The output contains the token; use it directly, e.g. `eval "$(hubctl env)"`.
    Env {
        /// Shell syntax to print
        #[arg(long, value_enum, default_value_t = Shell::Posix)]
        shell: Shell,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Shell {
    /// bash, zsh, sh
    Posix,
    Fish,
    /// Windows PowerShell / pwsh
    Powershell,
    /// Windows cmd.exe
    Cmd,
}

fn format_status(s: &StatusResponse) -> String {
    let mut out = String::new();
    out.push_str(&format!("hostname: {}\n", s.hostname));
    out.push_str(&format!("hub_version: {}\n", s.hub_version));
    for gpu in &s.gpus {
        out.push_str(&format!(
            "GPU {}: {}, VRAM {}/{} MiB, {} C, {} %\n",
            gpu.index,
            gpu.name,
            gpu.vram_used_mib,
            gpu.vram_total_mib,
            gpu.temperature_c,
            gpu.utilization_pct
        ));
    }
    for model in &s.models {
        let backend_str = match model.backend {
            Backend::Ollama => "ollama",
            Backend::Inferssd => "inferssd",
        };
        out.push_str(&format!(
            "model: {} ({}, {} MiB)\n",
            model.name, backend_str, model.vram_mib
        ));
    }
    if let Some(sch) = &s.scheduler {
        out.push_str(&format!(
            "scheduler: backend {}, queue {}, in flight {}, switches {}\n",
            sch.active_backend.as_deref().unwrap_or("-"),
            sch.queue_len,
            sch.in_flight,
            sch.switches_total
        ));
    }
    out
}

/// Zmienne środowiskowe kierujące narzędzia zgodne z OpenAI na hub.
fn format_env(server: &str, token: &str, shell: Shell) -> String {
    let base = format!("{}/v1", server.trim_end_matches('/'));
    let vars = [
        ("OPENAI_BASE_URL", base.as_str()),
        ("OPENAI_API_BASE", base.as_str()),
        ("OPENAI_API_KEY", token),
    ];
    let mut out = String::new();
    for (name, value) in vars {
        match shell {
            Shell::Posix => out.push_str(&format!("export {name}='{}'\n", value.replace('\'', "'\\''"))),
            Shell::Fish => out.push_str(&format!("set -gx {name} '{}'\n", value.replace('\'', "\\'"))),
            Shell::Powershell => out.push_str(&format!("$env:{name} = '{}'\n", value.replace('\'', "''"))),
            Shell::Cmd => out.push_str(&format!("set {name}={value}\n")),
        }
    }
    out
}

fn get_json(url: &str, token: &str) -> ureq::Response {
    match ureq::get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(401, _)) => {
            eprintln!("Request failed: server rejected the token (401)");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Request failed: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let base = cli.server.trim_end_matches('/').to_string();

    match cli.command {
        Commands::Status => {
            let response = get_json(&format!("{base}/status"), &cli.token);
            let status: StatusResponse = match response.into_json() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to parse response: {e}");
                    std::process::exit(1);
                }
            };
            if status.protocol_version != PROTOCOL_VERSION {
                eprintln!(
                    "Protocol version mismatch: server={}, expected={}",
                    status.protocol_version, PROTOCOL_VERSION
                );
                std::process::exit(2);
            }
            print!("{}", format_status(&status));
        }
        Commands::Models => {
            let response = get_json(&format!("{base}/v1/models"), &cli.token);
            let body: serde_json::Value = match response.into_json() {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Failed to parse response: {e}");
                    std::process::exit(1);
                }
            };
            print!("{}", format_models(&body));
        }
        Commands::Env { shell } => print!("{}", format_env(&base, &cli.token, shell)),
    }
}

/// Wypisuje identyfikatory modeli z odpowiedzi `/v1/models`, po jednym w linii.
fn format_models(body: &serde_json::Value) -> String {
    let mut ids: Vec<&str> = body["data"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["id"].as_str()).collect())
        .unwrap_or_default();
    ids.sort_unstable();
    ids.iter().map(|id| format!("{id}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_proto::{GpuStatus, LoadedModel};

    #[test]
    fn test_format_status() {
        let s = StatusResponse {
            protocol_version: PROTOCOL_VERSION,
            hostname: "test-host".into(),
            hub_version: "0.1.0".into(),
            gpus: vec![
                GpuStatus {
                    index: 0,
                    name: "NVIDIA RTX 3090".into(),
                    vram_total_mib: 24576,
                    vram_used_mib: 1024,
                    temperature_c: 45,
                    utilization_pct: 3,
                },
                GpuStatus {
                    index: 1,
                    name: "AMD RX 7900 XTX".into(),
                    vram_total_mib: 24576,
                    vram_used_mib: 2048,
                    temperature_c: 60,
                    utilization_pct: 77,
                },
            ],
            models: vec![
                LoadedModel {
                    name: "qwen".into(),
                    backend: Backend::Ollama,
                    vram_mib: 8000,
                },
                LoadedModel {
                    name: "sd-xl".into(),
                    backend: Backend::Inferssd,
                    vram_mib: 12000,
                },
            ],
            scheduler: None,
        };

        let output = format_status(&s);
        assert!(output.contains("hostname: test-host"));
        assert!(output.contains("hub_version: 0.1.0"));
        assert!(output.contains(
            "GPU 0: NVIDIA RTX 3090, VRAM 1024/24576 MiB, 45 C, 3 %"
        ));
        assert!(output.contains(
            "GPU 1: AMD RX 7900 XTX, VRAM 2048/24576 MiB, 60 C, 77 %"
        ));
        assert!(output.contains("model: qwen (ollama, 8000 MiB)"));
        assert!(output.contains("model: sd-xl (inferssd, 12000 MiB)"));
    }

    #[test]
    fn status_prints_scheduler_when_present() {
        let mut s = StatusResponse {
            protocol_version: PROTOCOL_VERSION,
            hostname: "h".into(),
            hub_version: "0.1.0".into(),
            gpus: vec![],
            models: vec![],
            scheduler: None,
        };
        assert!(!format_status(&s).contains("scheduler"));
        s.scheduler = Some(hub_proto::SchedulerStatus {
            queue_len: 2,
            active_backend: Some("ollama".into()),
            switches_total: 5,
            in_flight: 1,
        });
        assert!(format_status(&s)
            .contains("scheduler: backend ollama, queue 2, in flight 1, switches 5"));
    }

    #[test]
    fn env_posix_points_tools_at_v1_and_quotes_token() {
        let out = format_env("http://192.168.0.11:7878/", "to'ken", Shell::Posix);
        assert!(out.contains("export OPENAI_BASE_URL='http://192.168.0.11:7878/v1'\n"));
        assert!(out.contains("export OPENAI_API_BASE='http://192.168.0.11:7878/v1'\n"));
        assert!(out.contains("export OPENAI_API_KEY='to'\\''ken'\n"));
    }

    #[test]
    fn env_powershell_and_cmd_syntax() {
        let ps = format_env("http://h:7878", "t", Shell::Powershell);
        assert!(ps.contains("$env:OPENAI_API_KEY = 't'\n"));
        let cmd = format_env("http://h:7878", "t", Shell::Cmd);
        assert!(cmd.contains("set OPENAI_API_KEY=t\n"));
    }

    #[test]
    fn models_are_sorted_one_per_line() {
        let body = serde_json::json!({"data": [{"id": "b"}, {"id": "a"}, {"nope": 1}]});
        assert_eq!(format_models(&body), "a\nb\n");
        assert_eq!(format_models(&serde_json::json!({})), "");
    }
}
