# GPU Hub — status i zasady

Współdzielenie karty RTX 3090 serwera (192.168.0.11, Ubuntu 24.04, headless, sterownik 580)
z klientami Linux i Windows po Wi-Fi/LAN. Inspiracja: WiCi One (NetworkChuck, cFdoXn6yvE4).
Źródło: transkrypt filmu (Whisper, lokalnie; "WiCi" bywa zapisane jako "Wi-Fi/Y side"). Wiedza poniżej pochodzi z filmu sponsorowanego — liczby to deklaracje producenta.

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
- [x] E0a `hub-proto` (typy, testy przechodzą; autoryzacja domyślnie: token Bearer, TLS później)
- [x] E1 `hub-server` (agy; zweryfikowane lokalnie: 5 testów, clippy, e2e z hubctl na loopbacku; ścieżka NVML nieprzetestowana — brak GPU lokalnie): endpoint `/status` (nvidia-smi/NVML: GPU, VRAM, temp, obciążenie) + mDNS.
- [~] E2 `hub-client`: `hubctl status` gotowe (aider, zweryfikowane: test+clippy); `discover` (mDNS) czeka na E1
- [x] E3 proxy AI (agy; 9 testów, clippy czysty, zweryfikowane na .11 z prawdziwą Ollamą: auth, lista modeli, streaming SSE): pass-through do Ollamy + auth tokenem.
- [x] E4a scheduler + backend Ollama (agy zaczął, kwota agy wyczerpana w połowie; dokończone przez nadzorującego: podpięcie w main.rs, 6 testów schedulera, naprawione zakleszczenie dispatchu; 21 testów workspace, clippy czysty; zweryfikowane na .11 z 2 modelami)
- [x] E4b InferSSD jako drugi backend z wyłącznością na GPU (zrobione samodzielnie, bo kwota agy wyczerpana): zarządzanie procesem, sondaż gotowości, SIGTERM→SIGKILL, retry_after, wyłączność, zatrzymywanie po bezczynności, aliasy w /v1/models i /status. 25 testów hub-server (atrapa binarki), clippy czysty. Sprawdzone z PRAWDZIWĄ binarką inferssd na .11 (ścieżka błędu: brak modelu → 503 po 2 s, ponowna próba odrzucona w 13 ms). NIE sprawdzone: start z prawdziwym modelem (wymaga pobrania ~470 GiB). Przykład konfiguracji: docs/hub.example.toml
- [ ] E5 moduł stream: instalacja/parowanie Sunshine, Moonlight na kliencie.
- [ ] E6 Windows: build klienta (cross/CI), instalator.
- [ ] E7 badanie zdalnego CUDA.

## Zasady
- Podnieść wersję w Cargo.toml przed każdym push na GitHub.
- Bezpieczeństwo: tylko LAN/VPN, token + TLS; nie wystawiać portów na świat.

## Stan na 2026-10-31
Szkielet workspace'u (kompiluje się). Serwer: Ollama działa, Sunshine nie zainstalowany, brak sesji graficznej (tty).

## Ustalenia z filmu (WiCi One)
- **Zdalny GPU**: fałszywy sterownik GPU na kliencie (gra/Blender myślą, że mają lokalną 5090), wykonanie na serwerze. Sztuczki: komendy grupowane+kompresowane i wysyłane bez czekania na odpowiedź; dane tylko jako delta; odpowiedzi wysyłane z wyprzedzeniem (predykcja zapytań); wynik wracający jako skompresowane wideo. Efekt: ~8 GB/s → ~400 Mb/s, 92 FPS w Cyberpunku. Wi-Fi: ~1 Gb/s, RTT ~1,5 ms vs PCIe <1 µs. Wymaga sterownika jądra (Windows WDDM / Linux) — duży koszt, moduł `cuda` zostaje na koniec.
- **InferSSD** (github.com/wici-ai/inferssd, otwarty): MoE większe niż VRAM. (1) eksperci przepakowani do równych, wyrównanych chunków; (2) odczyt 64 chunków naraz (1 → 11,5 GB/s z SSD); (3) najpopularniejsi eksperci trzymani w VRAM/RAM (tylko ~1 GB/token z SSD); (4) podział obliczeń: GPU dla wag z VRAM, CPU dla wag z RAM/SSD. Wynik: ~8–15 tok/s dla DeepSeek V4.1 Flash (750B).
- **Scheduler**: modele "parkowane" na SSD zamiast ładowane od zera; przełączenie czat↔obraz 18 s → 8,6 s, obraz→czat 17 s → ~4 s.

## Serwer .11 (sprawdzone 2026-10-31)
RTX 3090 24 GB, 62 GB RAM, 32 wątki, NVMe 1,9 TB (94% zajęte, 112 GB wolne!) + NVMe 1 TB + HDD 1,8 TB. Mało miejsca na duże modele na głównym dysku — użyć drugiego NVMe.

## Konsekwencje dla planu
- E3/E4 (proxy AI + scheduler): parkowanie modeli = trzymanie w page cache/NVMe + `keep_alive`/`OLLAMA_MAX_LOADED_MODELS`; sprawdzić czy Ollama wystarcza, czy potrzebny własny loader.
- Nowy E3b: ocenić InferSSD na 3090 (licencja, wsparcie Ampere) vs llama.cpp z offloadem ekspertów (`--n-cpu-moe`). 62 GB RAM + NVMe pozwala na mniejsze MoE, nie na 750B.

## InferSSD — wynik rozpoznania (2026-10-31)
- Apache-2.0, nakładka na llama.cpp, serwer OpenAI-compatible (`inferssd serve`, :8080/v1). Testowane tylko: RTX 5060 Ti/5090, DGX Spark, M5 Max. **Ampere/3090 nie wymienione** (compute 8.6 — wymaga próby).
- Modele: DeepSeek-V4.1-Flash-1017 GGUF Q8_0 (~470 GiB, 763B/8–16B aktywnych) i Muse-Spark-Open UD-Q4_K_XL (816B). Wagi oryginalne: MIT.
- Deklarowane (5090): DeepSeek 7,27 tok/s, Muse 1,52 tok/s, 5060 Ti: 0,22 tok/s. Na 3090 + dysk PCIe3 spodziewać się mniej.
- Wymagania serwera .11: nvcc 12.0 OK, gcc/git/python OK, cgroup v2 OK, **brak liburing-dev (apt, wymaga sudo z hasłem)**. Dysk: nvme0 ma 112 GB wolnego; nvme1 (954 GB) zawiera partycje NTFS (Windows?) — do przeformatowania przez użytkownika.

## Licencje i szacunek dla autorów (obowiązkowe przed publikacją na GitHub)
- InferSSD: Apache-2.0 (LICENSE + NOTICE, copyright WiCi AI). llama.cpp/ggml: MIT. NIE GPL.
- Model integracji: InferSSD uruchamiany jako **osobny proces** (`inferssd serve`, API OpenAI), bez kopiowania ani wbudowywania jego kodu. Hub jest tylko klientem API, obok Ollamy.
- Jeśli kiedykolwiek skopiujemy lub zmodyfikujemy ich kod: dołączyć LICENSE i NOTICE, oznaczyć zmiany, zachować nagłówki praw autorskich.
- Nie używać nazwy/logo WiCi ani InferSSD do promocji hubu; wspominać o nich tylko faktograficznie ("obsługuje InferSSD").
- NOTICE InferSSD wymienia dodatkowe komponenty zewnętrzne — przeczytać w całości przed dystrybucją.
- Modele mają własne licencje (DeepSeek V4.1 Flash: MIT; Muse-Spark-Open i konwersje GGUF: sprawdzić osobno). Hub nie dołącza ani nie redystrybuuje wag.
- Film NetworkChuck: nie kopiować fragmentów, grafik ani transkryptu do repo; plik .mp4 jest w .gitignore. Opisy idei własnymi słowami.
- Własna licencja hubu: wybrać przed pierwszym publicznym push (propozycja: Apache-2.0 lub MIT) i dodać plik LICENSE.
- Przed publikacją: uruchomić przegląd zależności (cargo deny/licenses) i uzupełnić THIRD_PARTY_NOTICES.

## Postęp
- 2026-10-31: `hub-proto` gotowy (2 testy). Zlecenie E1 w `docs/tasks/E1-hub-server-status.md` (dla agy). InferSSD zainstalowany na .11 (bez modelu; test na miejscu, po zwolnieniu 600 GB–1 TB). Użytkownik przez VPN.
- 2026-10-31: hub-server wdrożony na .11 (`~/gpu-hub`, token w `~/gpu-hub/token`, uprawnienia 600); odczyt RTX 3090 przez NVML potwierdzony lokalnie na serwerze. Naprawiono ładowanie `libnvidia-ml.so.1`. Port 7878 otwarty w ufw (VPN 10.0.0.0/24, LAN 192.168.0.0/24) — zweryfikowane 2026-10-31: hubctl status i proxy działają z laptopa przez VPN. Binarki budowane `cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.39`. Dysk root .11: 582 GB wolnego.

## Drugi serwer (plan)
Ubuntu Server 24.04 LTS (spójność z .11); 2×NVMe w mdadm RAID0 na modele (przepustowość odczytu dla InferSSD, bez redundancji — wagi do pobrania ponownie), system na osobnym dysku, ext4/XFS (bez ZFS/btrfs), HDD na kopie/zimne modele. Hub powinien docelowo obsługiwać wiele hostów (lista, wybór najmniej obciążonego).
- 2026-10-31: E4a gotowe i wdrożone na .11. Znaleziony błąd: `acquire()` uruchamiał pętlę dispatchu w zadaniu żądającego → zakleszczenie przy dwóch zimnych modelach w kolejce (flaga `switching` blokowała cały hub). Poprawka: `spawn_dispatch()`; test regresji `cold_requests_for_several_models_do_not_deadlock` (potwierdzone: pada bez poprawki). Agy: kwota wyczerpana (reset za ~76 h od 2026-10-31); jego raporty "testy przechodzą" bywały nieprawdziwe/przedwczesne — zawsze weryfikować `cargo test --workspace` i `clippy --all-targets`. `herdr agent prompt --wait` może zwrócić przed końcem pracy agenta.
- Przełączenie modeli (przekroczenie vram_budget_mib) pokryte tylko testami na atrapie; na prawdziwej Ollamie niesprawdzone (2 małe modele mieszczą się razem).
- 2026-10-31: ufw dla 7878 skonfigurowane przez użytkownika; pełny test klient→VPN→hub→NVML/Ollama przeszedł. TODO: `hubctl status` ma wypisać pole `scheduler` (aider).
- 2026-10-31: E4b wdrożone na .11 (bez hub.toml → tylko Ollama; InferSSD włącza się wpisem [[backend]] kind="inferssd"). Potwierdzone na prawdziwej Ollamie: przełączenie modeli po przekroczeniu budżetu VRAM (qwen3.8 27B wyparł 2 małe, switches_total=1). Pułapka: `pkill -f <wzorzec>` w `ssh host '...'` zabija własną powłokę, jeśli wzorzec jest w jej linii poleceń — używać `pkill -x`.
- hubctl: dodane `models` i `env [--shell posix|fish|powershell|cmd]`, `status` pokazuje scheduler; docs/CLIENTS.md. hubctl.exe zbudowany (x86_64-pc-windows-gnu), nieprzetestowany na Windows. aider przez hub sprawdzony (naprawił błąd w pliku).
- Model do pobrania pod InferSSD: DeepSeek-V4.1-Flash-1017 Q8_0 ≈ 470 GiB; na .11 wolne 638 GB (/), RTX 3090 nieprzetestowana z modelem. Pobranie czeka na decyzję użytkownika (duże łącze/dysk).
