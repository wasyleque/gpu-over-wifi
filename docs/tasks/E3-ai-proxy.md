# E3 — hub-server: proxy AI do Ollamy

Cel: klienci (Open WebUI, aider, skrypty, aplikacje OpenAI-compatible) łączą się z hubem zamiast bezpośrednio z Ollamą; hub egzekwuje token i będzie miejscem scheduleru (E4).

## Wymagania
- W `crates/hub-server` dodać trasy `/v1/*` i `/api/*` jako przezroczysty reverse proxy do `--ollama-url` (domyślnie http://127.0.0.1:11434). Wszystkie metody, nagłówki (poza hop-by-hop), body.
- **Streaming**: odpowiedzi chunked/SSE/NDJSON (`stream: true`) muszą płynąć na bieżąco, bez buforowania całości. Bez ogólnego timeoutu na odpowiedź (tylko na nawiązanie połączenia, 5 s).
- **Autoryzacja**: ten sam token Bearer co `/status` (klienci OpenAI wysyłają `Authorization: Bearer`). Brak/zły token → 401, bez przekazywania zapytania dalej. Nagłówka `Authorization` NIE przekazywać do Ollamy.
- Gdy Ollama niedostępna: 502 z krótkim JSON `{"error":"backend unavailable"}`.
- Limit rozmiaru body żądania (domyślnie 64 MiB, `--max-body-mib`).
- Nie zmieniać istniejących tras `/hello`, `/status` ani ich testów. Refaktor trzymać w nowym module `proxy.rs`.
- Testy (bez prawdziwej Ollamy — lokalny atrapa-serwer axum w teście): 401 bez tokenu; przekazanie body i statusu; streaming chunków w kolejności; 502 przy martwym backendzie; brak przekazania Authorization do backendu.

## Poza zakresem
Scheduler/kolejka (E4), obsługa InferSSD jako drugiego backendu (osobny etap), TLS.

## Weryfikacja (zrobi nadzorujący)
`cargo test -p hub-server`, `cargo clippy -p hub-server -- -D warnings`, test na .11: `curl -H "Authorization: Bearer ..." http://.../v1/models`.

## Licencje
Tylko zależności MIT/Apache-2.0/BSD (patrz AGENTS.md).
