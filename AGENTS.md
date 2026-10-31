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

## Ustalenia z filmu (WiCi One)
- **Zdalny GPU**: fałszywy sterownik GPU na kliencie (gra/Blender myślą, że mają lokalną 5090), wykonanie na serwerze. Sztuczki: komendy grupowane+kompresowane i wysyłane bez czekania na odpowiedź; dane tylko jako delta; odpowiedzi wysyłane z wyprzedzeniem (predykcja zapytań); wynik wracający jako skompresowane wideo. Efekt: ~8 GB/s → ~400 Mb/s, 92 FPS w Cyberpunku. Wi-Fi: ~1 Gb/s, RTT ~1,5 ms vs PCIe <1 µs. Wymaga sterownika jądra (Windows WDDM / Linux) — duży koszt, moduł `cuda` zostaje na koniec.
- **InferSSD** (github.com/wici-ai/inferssd, otwarty): MoE większe niż VRAM. (1) eksperci przepakowani do równych, wyrównanych chunków; (2) odczyt 64 chunków naraz (1 → 11,5 GB/s z SSD); (3) najpopularniejsi eksperci trzymani w VRAM/RAM (tylko ~1 GB/token z SSD); (4) podział obliczeń: GPU dla wag z VRAM, CPU dla wag z RAM/SSD. Wynik: ~8–15 tok/s dla DeepSeek V4.1 Flash (750B).
- **Scheduler**: modele "parkowane" na SSD zamiast ładowane od zera; przełączenie czat↔obraz 18 s → 8,6 s, obraz→czat 17 s → ~4 s.

## Serwer .11 (sprawdzone 2026-10-31)
RTX 3090 24 GB, 62 GB RAM, 32 wątki, NVMe 1,9 TB (94% zajęte, 112 GB wolne!) + NVMe 1 TB + HDD 1,8 TB. Mało miejsca na duże modele na głównym dysku — użyć drugiego NVMe.

## Konsekwencje dla planu
- E3/E4 (proxy AI + scheduler): parkowanie modeli = trzymanie w page cache/NVMe + `keep_alive`/`OLLAMA_MAX_LOADED_MODELS`; sprawdzić czy Ollama wystarcza, czy potrzebny własny loader.
- Nowy E3b: ocenić InferSSD na 3090 (licencja, wsparcie Ampere) vs llama.cpp z offloadem ekspertów (`--n-cpu-moe`). 62 GB RAM + NVMe pozwala na mniejsze MoE, nie na 750B.
