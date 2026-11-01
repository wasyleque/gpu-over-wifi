# Optymalizacja DeepSeeka (InferSSD) na serwerze gpu-hub

Stan wyjściowy (zmierzony 2026-10-31): RTX 3090 Ti 24 GB, 60 GB RAM, jeden Kingston NVMe (PCIe 4, odczyt modelu z oryginalnych GGUF), kontekst domyślny: **3,76 tok/s** generowanie, 3,92 tok/s prompt.

## Zmierzone pokrętła
| Zmiana | Wynik | Wniosek |
|---|---|---|
| Wyłączenie rozumowania: `"chat_template_kwargs": {"enable_thinking": false}` (też `{"thinking": false}`) | pytanie krótkie: 81 s → 44 s, 237 → 139 tokenów, rozumowanie 0 | **Domyślnie wyłączać** dla generowania dokumentów i kodu. Włączać tylko dla trudnych decyzji. `thinking.type=disabled` nie działa; `reasoning_effort=low` skraca słabo. |
| Kontekst `--context 32768` | **nie startuje**: bufor obliczeń 29,8 GiB > 24 GB VRAM (`cudaMalloc failed`) | bufor rośnie liniowo z kontekstem |
| Kontekst `--context 16384` | startuje; stan ustalony 3,32 tok/s (≈ -12%) | akceptowalne dla długich promptów; pierwsza próbka (1,9) to rozgrzewka |
| Pole odpowiedzi | model zwraca myślenie w `reasoning_content`, odpowiedź w `content` | limit `max_tokens` obejmuje oba; przy włączonym rozumowaniu odpowiedź może się nie zacząć |

## Pokrętła z dokumentacji InferSSD (do zbadania, jeszcze niezmierzone)
- **Expert pack** (`expert.source` w pliku ustawień): eksperci pogrupowani po warstwie/ekspercie, wyrównani do 4 KiB; ~269 GiB dla DeepSeeka. Odpowiednik "przepakowania plików modelu pod SSD" z transkrypcji (tam 1 → 11,5 GB/s). Budowa: `engine/tools/expert-pack/create-moe-expert-pack.py '<shardy>' -o <pack> --align 4096` (NumPy + PyYAML). Uruchomienie z packiem wymaga `inferssd launch --settings`, nie `serve`.
- `prefetch.depth` (domyślnie 2), `vram.budget` / `vram.max_slots` / `vram.reserve_mib` (warstwa ekspertów na GPU konkuruje z buforem kontekstu), `cpu.decode_participants`, `cpu.io_cpus`, `host.headroom_mib`.
- Narzędzie samo mierzy maszynę przy starcie (CPU, RAM, GPU, dysk) i dobiera liczby wątków; ręcznie strojony jest głównie pack i prefetch.

## Plan eksperymentu (kolejność)
1. Poczekać na koniec konsultacji (dysk i GPU zajęte).
2. Wzorcowy pomiar bazowy: stały prompt, rozumowanie wyłączone, 200 tokenów, 3 powtórzenia.
3. Zbudować pack (NumPy/PyYAML w osobnym venv), zmierzyć czas i rozmiar.
4. `inferssd launch --execute --resources ... --model <shard1> --runtime-policy ... --settings s.ssd` z `expert.source=[pack]` na innym porcie; ten sam pomiar.
5. Jeśli zysk jest znaczący: dodać do huba `command = "launch"` + `settings` w `[[backend]]` i obsłużyć to w InferssdBackend (test na atrapie).
6. Zmierzyć `prefetch.depth` 2/3/4.
Kryterium: zapis liczb przed/po w tym pliku.

## Ograniczenia
- Zaleceń z dokumentacji nie traktujemy jako faktów o tym sprzęcie: benchmarki producenta robiono na RTX 5090/5060 Ti z dyskami PCIe 4/5 i 32 GB RAM.
- Wadliwy dysk Kingston (…6387) nie bierze udziału; po wymianie RAID0 może zmienić wyniki.

## WYNIKI (2026-11-01) — gpu-hub (RTX 3090 Ti, jeden Kingston NVMe, kontekst 16384, temperature 0, rozumowanie wyłączone, 3-4 powtórzenia)
| Konfiguracja | Generowanie | Prompt | Uwagi |
|---|---|---|---|
| `serve`, surowe GGUF (stan bazowy) | **3,37 tok/s** | 5,7 | całe 220 tokenów: 73-77 s |
| `launch`, GGUF, ubatch 2048 | 3,5 (po rozgrzewce) | 2,8-5,8 | +4%, w granicach szumu |
| `launch`, **expert pack**, ubatch 2048 | **4,76 tok/s** | **8,1** | **+41% / +40%**; 52 s na 220 tokenów |
| pack + `vram.budget` 18 GB / 21 GB | 4,41 / 4,45 | 8,0 | bez zysku: VRAM i tak zapełnia się do 24 GB po rozgrzaniu |
| pack + `prefetch.depth=4` | 4,56 | 8,0 | bez zysku (odczyt z dysku 10,2% → 8,4%, w szumie) |
| **przez hub** (`mode="launch"`, pack, ubatch 2048) | **4,66 tok/s** | **8,0** | wdrożone na gpu-hub |
| `launch` z ubatch/batch 16384 | serwer padł przy 1. żądaniu | | przyczyna nieustalona (`serve` z tymi liczbami działał) |

Telemetria (na token, warstwy ekspertów): GGUF 783 ms (oczekiwanie na dysk 599 ms = 77%); **pack 377 ms (197 ms = 52%)** — pack skrócił czekanie na dysk 3x; obliczenia ~140 ms bez zmian; trafienia VRAM ~68%, RAM ~20-24%, dysk ~8-13%.

## Wnioski
- **Expert pack to jedyna zmiana z istotnym efektem** (+36-41%). Odpowiada trikowi "przepakowanie plików modelu pod SSD" z transkrypcji.
- Pozostałe ~50% czasu to czekanie na jeden dysk. Następny skok da sprzęt: RAID0 z dwóch Kingstonów po wymianie wadliwego (odczyt ~2x) — wtedy przebudować/skopiować model i pack na macierz.
- Wdrożenie: hub `mode = "launch"` (patrz `docs/hub.example.toml`), pack 269 GiB w `/srv/models/DeepSeek-V4.1-Flash-1017/model.expert-pack`, plik ustawień `~/gpu-hub/inferssd-settings.ssd`. Budowa packa: 9 min 8 s (~650 MB/s).
- Rozumowanie wyłączać parametrem żądania `chat_template_kwargs.enable_thinking=false` dla generowania dokumentów i kodu.

## Stabilność na długim prompcie (2026-11-01) — WAŻNA KOREKTA
Konfiguracja "pack + ubatch 2048 + vram auto" była szybka na krótkich promptach, ale **padała** na długich zleceniach (prompt ~4 tys. tokenów, generowanie >200 tokenów): `CUDA graph instantiate ran out of device memory`, `[kimi_vram_tier] cannot drain after out of memory`, serwer znika w trakcie odpowiedzi. Przyczyna: warstwa ekspertów w VRAM (`vram.budget=auto`) zapełnia kartę do końca i nie zostawia miejsca na grafy CUDA.
| Zapas VRAM | prompt 4105 tok | błędy OOM w logu | gen |
|---|---|---|---|
| brak | **serwer pada** | 26 | |
| `vram.reserve_mib=2048` + `vram.margin_mib=2112` | OK, 55,7 tok/s | 0 | 3,02 tok/s |
| 4096 / 4160 | OK, 55,9 tok/s | 0 | 3,16 tok/s |
**Wdrożone:** `args = ["--set","vram.reserve_mib=2048","--set","vram.margin_mib=2112"]` w `[[backend]]` (margin musi być większy od reserve o >= 64 MiB, inaczej `launch` odmawia startu). Przetwarzanie długiego promptu: ok. 56 tok/s (krótkie prompty ok. 8 tok/s, bo każdy token czyta eksperty osobno). Generowanie przy długim kontekście ok. 3,0-3,1 tok/s (przy krótkim 4,7).
**Lekcja:** pomiary prędkości robić także na długim prompcie i długiej generacji, nie tylko na 60 tokenach.
