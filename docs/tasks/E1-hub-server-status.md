# E1 — hub-server: GET /status, GET /hello

Cel: demon na serwerze (Ubuntu 24.04, RTX 3090) udostępniający stan GPU i modeli.

## Wymagania
- Crate `crates/hub-server`, zależność `hub-proto` (typy: `StatusResponse`, `GpuStatus`, `LoadedModel`, `Hello`, stałe `DEFAULT_PORT`, `AUTH_HEADER`).
- HTTP (axum + tokio), nasłuch domyślnie `0.0.0.0:7878` (`--listen` / env `HUB_LISTEN`).
- `GET /hello` — bez autoryzacji, zwraca `Hello`.
- `GET /status` — wymaga `Authorization: Bearer <token>` (token z env `HUB_TOKEN` albo pliku `--token-file`; brak tokenu = odmowa startu). Zwraca `StatusResponse`. Porównanie tokenu w stałym czasie.
- Dane GPU: crate `nvml-wrapper` (NVML); jeśli NVML niedostępny, `gpus` puste + log ostrzeżenia, serwer nadal działa.
- Modele: `GET http://127.0.0.1:11434/api/ps` (Ollama) → `LoadedModel{backend: Ollama}`; błąd/brak Ollamy = pusta lista, bez paniki.
- Ogłaszanie mDNS (`MDNS_SERVICE`) — crate `mdns-sd`.
- Testy: autoryzacja (401 bez tokenu, 200 z tokenem), serializacja zgodna z `hub-proto`, parser odpowiedzi `/api/ps` na przykładowym JSON.

## Poza zakresem
Proxy AI, scheduler, Sunshine, TLS (osobne etapy).

## Weryfikacja (zrobi nadzorujący)
`cargo test -p hub-server`, `cargo clippy -p hub-server -- -D warnings`, uruchomienie na .11 i `curl -H "Authorization: Bearer ..." http://192.168.0.11:7878/status`.

## Licencje
Tylko zależności MIT/Apache-2.0/BSD; nie kopiować kodu z innych projektów bez zachowania licencji (patrz AGENTS.md).
