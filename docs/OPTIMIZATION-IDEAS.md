# Rejestr pomysłów optymalizacyjnych i wyników (prior art)

Każdy pomysł, który może dać korzyść w *jakimkolwiek* scenariuszu (klient, łącze, gra), jest tu zapisany z datą, hipotezą, sposobem pomiaru i wynikiem
(także negatywnym). Wpisy powstają przed wdrożeniem, a wyniki są dopisywane w kolejnych commitach; historia git jest dowodem chronologii.
Licencja projektu: FSL-1.1-ALv2 (od 0.3.0).

Format: **ID — pomysł** (data pomysłu, autor/źródło) · hipoteza · pomiar · status/wynik.

## Cel i kontekst (2026-11-19)

Klientem docelowym jest słaby komputer (Celeron/Pentium/stary laptop bez karty graficznej); mocny klient z własnym GPU nie potrzebuje zdalnego. Procesor klienta jest budżetem:
każdy pomiar raportujemy także jako zużycie CPU klienta na klatkę. Gry, w których CPU klienta jest sufitem, dokumentujemy jako granicę. Docelowo: współdzielenie GPU serwera
przez kilku klientów (S1 niżej).

- **S1 — współdzielenie GPU serwera przez kilku klientów** (2026-11-19, user) · Do zmierzenia: równoległe sesje NVENC (limit sterownika), VRAM przy kilku sesjach i po rozłączeniu,
  uczciwość szeregowania (FPS każdego klienta przy dwóch grach naraz), łącze serwera. Test: dwie gry naraz (minipc + DESKTOP). · Status: do zmierzenia po przyjściu karty 2,5G do DESKTOPA.

## Kodowanie zapisów pamięci

- **C1 — zstd w trybie "fast" (poziomy ujemne) z prefiksem lustra** (2026-11-18, Claude, w trakcie konsultacji z DeepSeek) · Hipoteza: na słabym kliencie z szybkim
  łączem (minipc N6000, 2,5 GbE) kodowanie z prefiksem zachowuje niskie bajty na łączu (~7 GB vs 16 GB LZ4), a poziom -3..-7 obniża koszt CPU
  do poziomu LZ4. · Pomiar: `GPU_ICD_ZSTD_LEVEL=-1|-3|-5|-7` (bez `GPU_ICD_MEM_LZ4`), Rise of the Tomb Raider, min. 2 przebiegi, FPS + bajty na łączu + CPU wątków frame.
  · Status: **zmierzone -3: 14,47 FPS, hipoteza niepotwierdzona** (Run 42); wniosek: przewagę LZ4 daje brak lustra, nie szybkość kompresora. C1b (zstd -3 bez prefiksu) zmierzone w Run 43: gorsze (Syria 10,6); LZ4 pozostaje najlepsze na N6000 (Run 44, Syria 15,1). Przyczyna przewagi LZ4 nie jest rozpoznana.
- **C2 — zastąpienie `lz4_flex` kompresorem C lz4 / trybem fast** (2026-11-18, Claude) · Hipoteza: niższy koszt CPU przy podobnym współczynniku. · Status: do zmierzenia offline.
- **C3 — kodek różnic bez kompresji (XOR + RLE, SIMD)** (2026-11-18, DeepSeek v4-pro, wariant) · Wcześniejszy pomiar offline (Run 30, 2026-11-0x): XOR+zstd1 o 10-50% gorszy od zstd z prefiksem
  w rozmiarze; nie zmierzono jeszcze kosztu CPU samego XOR+RLE. · Status: do zmierzenia offline (rozmiar i ns/MB).
- **C4 — adaptacyjny regulator kodeka (histereza wg kolejki wysyłki i CPU)** (2026-11-18, DeepSeek + user) · Zalążek: `GPU_ICD_RAW_BELOW` (Run 36-40: bez zysku
  przy progu 2/16 MB). · Status: wymaga lepszego sygnału (wykorzystanie łącza w oknie, nie sama kolejka).

## Klatki i dekodowanie

- **V1 — konwersja NV12->BGRA przez VA-API VPP na iGPU klienta** (2026-11-18, DeepSeek) · Hipoteza: -3..5% CPU klienta. · Status: do zrobienia.
- **V2 — szybsza konwersja NV12->BGRA na CPU (auto-wektoryzacja)** (2026-11-18, kod: DeepSeek flash) · Wynik: 8,7 -> 5,6 ms/klatkę 1080p na N6000, bit w bit. · Status: **wdrożone** (0.3.13).

## Architektura i system (każdy pomysł ma test; "odrzucone" wolno wpisać dopiero po wyniku)

- **A1 — pipeline bez blokady instancji (etapy capture/encode/send, kolejki SPSC)** (2026-11-18, DeepSeek) · Wysokie ryzyko poprawności, więc najpierw pomiar górnej granicy zysku:
  z istniejących liczników (trzymanie blokady 41 s, w tym skan `PAGEMAP_SCAN` 23-24 s, na ~220 s przebiegu) wyliczyć, ile czasu ściany da się odzyskać przy idealnym
  zrównolegleniu (prawo Amdahla), potem prototyp za przełącznikiem z włączonym audytem lustra (pełne porównanie pamięci co N klatek, 0 przeoczeń). · Status: do zmierzenia.
- **A2 — prefetch / diff deskryptorów** (2026-11-18, DeepSeek) · Pomiar: liczba chybień cache deskryptorów na grę (TR: 4330 obiegów/przebieg, Cyberpunk: >20 000 przed zawieszeniem)
  razy 250 µs = górna granica zysku; prototyp: wysyłka szablonu przy tworzeniu PSO. · Status: do zmierzenia.
- **A3 — spekulatywne zwalnianie semaforów** (2026-11-18, DeepSeek) · Ryzyko poprawności, dlatego test w **trybie cienia**: klient przewiduje wynik i zapisuje przewidywanie,
  ale nadal czeka na serwer; zliczamy trafienia i pomyłki. Dopiero przy 0 pomyłek na wielu grach rozważamy działanie na przewidywaniu (z audytem). Uwaga: częściowo już istnieje
  ("oczekiwania zwrócone wcześniej" dla vkd3d, ~105 tys. na przebieg Cyberpunka). · Status: do zbudowania (tryb cienia).
- **A4 — soft-dirty (`clear_refs` + bit 55 pagemap) zamiast `PAGEMAP_SCAN`** (2026-11-18, DeepSeek) · Zmierzone (`docs/measurements/2026-11-18-scan`): soft-dirty 25-30x wolniejszy
  od `PAGEMAP_SCAN` na N6000 i na Ryzenie; przy THP=always nadmiarowe strony (granulacja 2 MiB). · Status: **zmierzone, gorsze od obecnej metody**.
- **A5 — userfaultfd write-protect** (2026-11-18, DeepSeek) · To jest dokładnie nasza obecna metoda (`memtrack.rs`: uffd WP_ASYNC + `PAGEMAP_SCAN`); najszybsza z czterech zmierzonych
  (hash 300-400x, memcmp 150-300x wolniejsze). · Status: **wdrożone od dawna; potwierdzone pomiarem**.
- **A10 — węższy skan: tylko zakresy, które mogą być zmienione, i/lub rzadziej** (2026-11-18, Claude, z wyniku A4/A5) · Koszt skanu `PAGEMAP_SCAN` rośnie z rozmiarem obszaru
  (~0,5 µs/MiB); w grze ok. 0,9 ms x ~5 na klatkę pod blokadą instancji (23 s z 220 s przebiegu). Pomiar: rozkład rozmiarów zakresów w skanach `take_dirty_all` vs rzeczywiście zmienione
  strony w grze (nowy licznik), potem prototyp skanujący tylko "gorące" zakresy. · Status: do zmierzenia.
- **A6 — wysyłka `MSG_ZEROCOPY` / io_uring** (2026-11-18, DeepSeek) · Mikrobenchmark (`tools/microbench/zc`), kabel 2,5G minipc->serwer, 2 GB na próbę, CPU = user+sys procesu:
  `write` 0,27-0,41 s/GB vs `MSG_ZEROCOPY` 0,03-0,10 s/GB (5-10x mniej) przy ramkach >= 512 KB, przepustowość równa (~290 MB/s); przy 64 KB zerocopy wolniejsze (249 vs 292 MB/s).
  Dla ~16-35 GB na przebieg to oszczędność ok. 5 s CPU (~0,5% mocy minipc): **mały zysk, nie tłumaczy 17% jądra z profilu**. io_uring niezmierzone. · Status: zmierzone, wdrożenie niskiego priorytetu (tylko dla dużych ramek).
- **A7 — pula buforów zamiast świeżych alokacji w wątku wysyłki** (2026-11-18, Claude, z profilu: `clear_pages` i `rep_movs` w `gpu-icd-send`) · Pomiar: perf przed/po. · Status: do zrobienia.
- **A8 — sortowanie zakresów różnic przed pakowaniem** (2026-11-18, DeepSeek) · Pomiar offline na nagraniu zapisów (rozmiar po kodowaniu, liczba ramek). · Status: do zmierzenia.
- **A9 — VPP na iGPU dla NV12->BGRA** (patrz V1).

## Wyniki zmierzonych eksperymentów

- 2026-11-18, minipc N6000: LZ4 zamiast zstd = +6 FPS w Tomb Raiderze (14-17,5 -> ~20 FPS); surowe zapisy przy pustej kolejce = brak zysku (ponad łącze w ciężkich scenach).
  Szczegóły: `docs/measurements/2026-11-05-rottr/README.md`, Runs 35-41.
