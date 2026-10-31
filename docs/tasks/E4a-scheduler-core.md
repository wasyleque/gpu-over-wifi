# E4a — hub-server: rdzeń schedulera + backend Ollama

Cel: zamiast ślepego proxy (E3) hub rozumie, do którego backendu i modelu trafia zapytanie, kolejkuje je i ogranicza liczbę przełączeń modeli na jednej karcie (RTX 3090 24 GB).

## Architektura
- Nowy moduł `scheduler.rs` (+ `backend.rs`). `proxy.rs` zostaje transportem; decyzję "dokąd" podejmuje scheduler.
- Trait `Backend` (async): `name()`, `base_url()`, `models() -> Vec<String>` (jakie modele obsługuje), `resident() -> Vec<String>` (co jest załadowane), `ensure_ready(model)` (np. rozgrzanie), `unload_all()` (zwolnienie VRAM), `class() -> ResidencyClass`.
- `ResidencyClass`: `Shared` (Ollama — wiele małych modeli naraz) lub `Exclusive` (InferSSD — zajmuje całą kartę; używane w E4b, tu tylko typ + testy z atrapą).
- Implementacja `OllamaBackend`: modele z `/api/tags`; załadowane z `/api/ps`; `unload_all` = `POST /api/generate {"model":..,"keep_alive":0}` dla każdego załadowanego.

## Routing
- Model brany z body JSON (`model`) dla `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/api/chat`, `/api/generate`, `/api/embed*`. Body buforować tylko do limitu (np. 1 MiB) na potrzeby odczytu `model`, resztę przekazać bez zmian.
- Brak pola `model` lub trasy bez modelu (`/v1/models`, `/api/tags`, `/api/ps`, `/api/show` itd.) → przekazać do domyślnego backendu (Ollama) bez kolejkowania.
- Nieznany model → 404 JSON `{"error":"unknown model"}`.

## Kolejka i polityka
- Globalna kolejka żądań generacji. Zasada: **grupować po modelu** — jeśli model jest już załadowany, żądania do niego wyprzedzają te wymagające przełączenia, ale z **agingiem**: żądanie czekające dłużej niż `max_wait_secs` (domyślnie 120) dostaje pierwszeństwo, by nie głodzić innych modeli.
- Limit współbieżności na model (`max_concurrent_per_model`, domyślnie 4); powyżej — kolejka.
- Przy przełączeniu (nowy model nie mieści się obok załadowanych — dla Ollamy: suma `size_vram` + szacunek > `vram_budget_mib`) scheduler czeka aż aktywne żądania do wypartych modeli się skończą (drain), potem `unload`.
- Rezygnacja klienta (zamknięcie połączenia) usuwa żądanie z kolejki.
- Konfiguracja: plik `hub.toml` (`--config`, domyślnie obok binarki) z sekcjami `[scheduler]` (max_wait_secs, max_concurrent_per_model, vram_budget_mib) i `[[backend]]`; flagi CLI z poprzedniego etapu (`--ollama-url`) zostają i nadpisują.

## Obserwowalność
- `hub-proto`: dodać `SchedulerStatus { queue_len: u32, active_backend: Option<String>, switches_total: u64, in_flight: u32 }` oraz pole `scheduler: Option<SchedulerStatus>` w `StatusResponse` z `#[serde(default)]` (zmiana addytywna, `PROTOCOL_VERSION` bez zmian; istniejące testy muszą dalej przechodzić).
- `GET /status` wypełnia to pole.

## Testy (bez prawdziwej Ollamy — atrapy)
- Routing po polu `model` (chat, generate, embeddings) i fallback bez modelu.
- Nieznany model → 404.
- Grupowanie: 3 żądania A, 1 żądanie B, 1 żądanie A → kolejność wykonania minimalizuje przełączenia.
- Aging: żądanie B czekające ponad `max_wait_secs` (czas wstrzykiwany/mockowany) wyprzedza kolejne A.
- Limit współbieżności.
- Anulowanie: porzucone żądanie znika z kolejki.
- `/status` zawiera `scheduler`; stary JSON bez tego pola nadal się parsuje.
- Streaming dalej płynie (testy E3 muszą przechodzić bez zmian semantyki).

## Poza zakresem
InferSSD (E4b), TLS, uwierzytelnianie per-użytkownik.

## Weryfikacja (nadzorujący)
`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, test na .11 z prawdziwą Ollamą (przełączanie dwóch małych modeli, podgląd `/status`).

## Zasady
Małe commity nie są wymagane (nadzorujący commituje). Tylko zależności MIT/Apache-2.0/BSD. Nie ruszać istniejących tras `/hello` i `/status` poza dodaniem pola.
