# GPU Hub — status i zasady

Współdzielenie karty RTX 3090 serwera (192.168.0.11, Ubuntu 24.04, headless, sterownik 580)
z klientami Linux i Windows po Wi-Fi/LAN. Inspiracja: WiCi One (NetworkChuck, cFdoXn6yvE4).
Uwaga: nie udało się pobrać transkryptu filmu (YT 429) — wnioski z opisu, nie z treści.

## Architektura (3 moduły, od najłatwiejszego)
1. **stream** — gry/desktop 3D: Sunshine (serwer) + Moonlight (klient). Hub automatyzuje instalację, parowanie, profile.
2. **ai** — proxy zgodne z OpenAI przed Ollamą (:11434), kolejka zadań + scheduler modeli (unikanie ciągłego przeładowywania VRAM), autodiscovery, ustawianie `OPENAI_BASE_URL` / `OLLAMA_HOST` na kliencie.
3. **cuda** (eksperymentalne, na końcu) — zdalne CUDA/Vulkan; po Wi-Fi wolne, wymaga badań (SCUDA/Cricket).

## Crates
- `hub-proto` — wspólne typy (discovery, sesje, status GPU).
- `hub-server` — demon na .11: status GPU (NVML), zarządzanie Sunshine, proxy AI.
- `hub-client` — CLI/GUI Linux + Windows: discovery, parowanie, konfiguracja klienta.

## Etapy (małe, weryfikowalne; logika/integracje → agy, proste → aider+Ollama)
- [ ] E0 PoC ręczny: zainstalować Sunshine na .11 (wymaga sesji graficznej/headless X lub wlroots + dummy display), sprawdzić Moonlight z laptopa.
- [ ] E1 `hub-server`: endpoint `/status` (nvidia-smi/NVML: GPU, VRAM, temp, obciążenie) + mDNS.
- [ ] E2 `hub-client`: `hubctl discover|status`.
- [ ] E3 proxy AI: pass-through do Ollamy + auth tokenem.
- [ ] E4 scheduler: kolejka żądań, grupowanie po modelu, polityka keep_alive.
- [ ] E5 moduł stream: instalacja/parowanie Sunshine, Moonlight na kliencie.
- [ ] E6 Windows: build klienta (cross/CI), instalator.
- [ ] E7 badanie zdalnego CUDA.

## Zasady
- Podnieść wersję w Cargo.toml przed każdym push na GitHub.
- Bezpieczeństwo: tylko LAN/VPN, token + TLS; nie wystawiać portów na świat.

## Stan na 2026-10-31
Szkielet workspace'u (kompiluje się). Serwer: Ollama działa, Sunshine nie zainstalowany, brak sesji graficznej (tty).
