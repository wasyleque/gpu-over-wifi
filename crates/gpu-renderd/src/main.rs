//! `gpu-renderd`: demon zdalnego GPU. Użycie:
//!   gpu-renderd --token-file <plik> [--listen <adres:port>] [--max-conns <n>]
//! Domyślnie nasłuchuje tylko na 127.0.0.1:7979; wystawienie w sieci wymaga jawnego `--listen`.

use std::net::TcpListener;

use gpu_renderd::server;

fn usage() -> ! {
    eprintln!("Użycie: gpu-renderd --token-file <plik> [--listen <adres:port>] [--max-conns <n>]");
    std::process::exit(2);
}

fn main() {
    let mut listen = "127.0.0.1:7979".to_string();
    let mut token_file: Option<String> = None;
    let mut max_conns = 16usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => listen = args.next().unwrap_or_else(|| usage()),
            "--token-file" => token_file = Some(args.next().unwrap_or_else(|| usage())),
            "--max-conns" => max_conns = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    let Some(token_file) = token_file else { usage() };
    let token = match std::fs::read_to_string(&token_file) {
        Ok(t) => t.trim().as_bytes().to_vec(),
        Err(e) => {
            eprintln!("Nie można odczytać {token_file}: {e}");
            std::process::exit(1);
        }
    };
    if token.is_empty() {
        eprintln!("Token w {token_file} jest pusty.");
        std::process::exit(1);
    }
    // Klient nie może wymusić warstw walidacyjnych ani cudzych bibliotek: czyścimy środowisko, zanim
    // powstanie jakikolwiek wątek (backend nigdy nie przekazuje nazw warstw do vkCreateInstance).
    for var in ["VK_INSTANCE_LAYERS", "VK_LAYER_PATH", "VK_ADD_LAYER_PATH", "VK_LOADER_LAYERS_ENABLE"] {
        std::env::remove_var(var);
    }
    let listener = match TcpListener::bind(&listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Nie można nasłuchiwać na {listen}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("gpu-renderd nasłuchuje na {listen} (limit {max_conns} połączeń)");
    server::run(listener, token, max_conns);
}
