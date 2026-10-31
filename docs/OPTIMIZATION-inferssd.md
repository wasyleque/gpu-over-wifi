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
