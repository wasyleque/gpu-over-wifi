# E4b — InferSSD jako drugi backend (wyłączność na GPU)

Zależy od E4a. Cel: hub zarządza procesem `inferssd serve` i kieruje do niego zapytania o skonfigurowane modele, pilnując, żeby nie działał jednocześnie z modelami Ollamy w VRAM.

## Fakty o InferSSD (z README, bez jeszcze działającego modelu na 3090)
- Uruchamiany jako osobny proces: `inferssd serve -m /ścieżka/pierwszy-shard.gguf --port 8080 [--model-dir ...]`, API zgodne z OpenAI pod `http://127.0.0.1:8080/v1`.
- "Używa całej dostępnej pamięci" — traktować jako `Exclusive`.
- Start (wczytanie + profilowanie) trwa długo (minuty); potrzebny sondaż gotowości, nie sleep.
- Jeden model na proces.

## Wymagania
- `InferssdBackend` w `backend.rs` implementujący trait z E4a, `class() = Exclusive`.
- Konfiguracja w `hub.toml`:
  ```toml
  [[backend]]
  kind = "inferssd"
  binary = "/home/wasyl/.local/bin/inferssd"
  port = 8080
  idle_stop_secs = 900
  [[backend.model]]
  alias = "deepseek-v4.1-flash"     # nazwa widoczna dla klientów
  path  = "/ścieżka/do/pierwszy-shard.gguf"
  ```
- Zarządzanie procesem: start na żądanie (`tokio::process`, `kill_on_drop`, własna grupa procesów), odczyt stdout/stderr do logu, sondaż `GET /v1/models` co 2 s aż do gotowości (limit `start_timeout_secs`, domyślnie 1800), w trakcie startu żądania czekają w kolejce (klient dostaje opóźnioną odpowiedź, nie błąd).
- Wyłączność: przed startem InferSSD scheduler robi drain + `unload_all` Ollamy i **nie dopuszcza** żądań Ollamy, dopóki InferSSD działa; przy żądaniu do Ollamy w trakcie działania InferSSD: kolejka → po `idle_stop_secs` bezczynności lub gdy kolejka Ollamy czeka dłużej niż `max_wait_secs` — zatrzymaj InferSSD (SIGTERM, po 15 s SIGKILL) i przełącz.
- Awaria procesu (wyjście z kodem ≠ 0): oznaczyć backend jako `degraded`, zwrócić 503 z JSON, nie próbować restartu w pętli (max 1 ponowna próba, potem czekać na ręczną interwencję lub kolejne żądanie po `retry_after_secs`).
- `/v1/models` zwraca modele wszystkich backendów (aliasy InferSSD z prefiksem `inferssd/`? — **nie**: używać aliasu bez prefiksu, ale zadbać o unikalność względem tagów Ollamy; konflikt = błąd konfiguracji przy starcie).
- `StatusResponse.models`: załadowany model InferSSD jako `LoadedModel{backend: Inferssd, vram_mib: ...}` (vram z NVML, jeśli dostępne; inaczej 0).

## Testy (atrapy — bez prawdziwego InferSSD)
- Atrapa binarki (skrypt/mały program testowy) symulująca opóźniony start i API: sondaż gotowości, żądania czekają w trakcie startu.
- Wyłączność: żądanie InferSSD wymusza unload Ollamy (atrapa Ollamy rejestruje wywołania `keep_alive:0`); żądanie Ollamy podczas pracy InferSSD czeka, potem przełącza po idle.
- Awaria procesu → 503, brak pętli restartów.
- Konflikt aliasów → błąd startu z czytelnym komunikatem.
- Zatrzymanie: SIGTERM, a po timeout SIGKILL (atrapa ignorująca SIGTERM).

## Poza zakresem
Pobieranie modeli, kwantyzacja, TLS, test na prawdziwych wagach (robi użytkownik na miejscu).

## Weryfikacja (nadzorujący)
`cargo test --workspace`, clippy `-D warnings`; test z prawdziwym InferSSD dopiero po zwolnieniu miejsca i pobraniu modelu.

## Licencje
InferSSD wywoływany wyłącznie jako zewnętrzny proces (Apache-2.0, patrz AGENTS.md); nie kopiować jego kodu.
