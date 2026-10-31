# E5b — hub-server: rezerwacje GPU i API sesji gry

Zależy od E5a (usługi na serwerze) i istniejącego schedulera (E4a/E4b). Szczegóły: `docs/DESIGN-game-streaming.md` §4.

## Wymagania
- `hub-proto`: `GameSessionState { active, started_secs, encoder_sessions, vram_used_mib }`, `GameSessionRequest { app, width, height, fps, codec }`, `GameSessionInfo { host, ports, apps }`. Zmiany addytywne (`#[serde(default)]`), `PROTOCOL_VERSION` bez zmiany.
- Scheduler: `reserve_gpu(owner, vram_mib)` / `release_gpu(owner)`. Rezerwacja (a) wyładowuje InferSSD i blokuje jego start (503 z komunikatem), (b) zmniejsza `vram_budget_mib` Ollamy o rezerwację, (c) wyładowuje modele Ollamy, które się już nie mieszczą.
- Moduł `game.rs`: start/stop usług przez `systemctl --user` (konfigurowalne nazwy jednostek w `hub.toml` sekcja `[game]`), sondaż gotowości Sunshine (port 47989), odczyt liczby aktywnych sesji NVENC przez NVML (`nvmlDeviceGetEncoderStats`/sesje), wykrywanie bezczynności (`game_idle_secs`, domyślnie 300) i automatyczny koniec sesji.
- Trasy: `POST/GET/DELETE /v1/game/session` (token Bearer jak reszta). Tylko jedna sesja naraz.

## Testy (atrapy, bez prawdziwego Sunshine i GPU)
- Start rezerwuje VRAM i zatrzymuje InferSSD; stop przywraca limit.
- Żądanie do InferSSD w trakcie sesji → 503 z komunikatem; po sesji działa.
- Bezczynność (wstrzykiwany zegar + atrapa licznika NVENC = 0) kończy sesję.
- Druga sesja w trakcie pierwszej → 409.
- Awaria startu usługi → sesja nie jest aktywna, rezerwacja zwolniona.

## Poza zakresem
Parowanie Sunshine, TLS, wiele sesji, Windows.

## Licencje
Sunshine tylko jako osobny proces (GPL-3.0); nie kopiować jego kodu.
