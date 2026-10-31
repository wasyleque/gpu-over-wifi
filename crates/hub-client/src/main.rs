use clap::{Parser, Subcommand};
use hub_proto::{Backend, PROTOCOL_VERSION, StatusResponse};

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
    /// Show hub status
    Status,
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
    out
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => {
            let url = format!("{}/status", cli.server.trim_end_matches('/'));
            let result = ureq::get(&url)
                .set("Authorization", &format!("Bearer {}", cli.token))
                .call();

            match result {
                Ok(response) => {
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
                Err(e) => {
                    eprintln!("Request failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
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
}
