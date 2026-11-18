# Rejestr pomysłów optymalizacyjnych i wyników (prior art)

Każdy pomysł, który może dać korzyść w *jakimkolwiek* scenariuszu (klient, łącze, gra), jest tu zapisany z datą, hipotezą, sposobem pomiaru i wynikiem
(także negatywnym). Wpisy powstają przed wdrożeniem, a wyniki są dopisywane w kolejnych commitach; historia git jest dowodem chronologii.
Licencja projektu: FSL-1.1-ALv2 (od 0.3.0).

Format: **ID — pomysł** (data pomysłu, autor/źródło) · hipoteza · pomiar · status/wynik.

## Kodowanie zapisów pamięci

- **C1 — zstd w trybie "fast" (poziomy ujemne) z prefiksem lustra** (2026-11-18, Claude, w trakcie konsultacji z DeepSeek) · Hipoteza: na słabym kliencie z szybkim
  łączem (minipc N6000, 2,5 GbE) kodowanie z prefiksem zachowuje niskie bajty na łączu (~7 GB vs 16 GB LZ4), a poziom -3..-7 obniża koszt CPU
  do poziomu LZ4. · Pomiar: `GPU_ICD_ZSTD_LEVEL=-1|-3|-5|-7` (bez `GPU_ICD_MEM_LZ4`), Rise of the Tomb Raider, min. 2 przebiegi, FPS + bajty na łączu + CPU wątków frame.
  · Status: kod gotowy (filtr poziomów rozszerzony do -7..19), pomiar w toku.
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
- **A4 — soft-dirty (`clear_refs` + bit 55 pagemap) zamiast `PAGEMAP_SCAN`** (2026-11-18, DeepSeek) · Mikrobenchmark: koszt jednego skanu 4-64 MB przy 1-1000 brudnych stronach
  (dziś ok. 0,92 ms/skan, ~5 na klatkę), oba mechanizmy na tym samym syntetycznym obciążeniu, na N6000 i na Ryzenie. · Status: do zmierzenia.
- **A5 — userfaultfd write-protect dla małych buforów** (2026-11-18, DeepSeek) · Mikrobenchmark: koszt obsłużonego page-fault vs koszt skanowania przy typowej liczbie
  zapisów na klatkę (z `GPU_ICD_STATS`). · Status: do zmierzenia.
- **A6 — wysyłka `MSG_ZEROCOPY` / io_uring** (2026-11-18, DeepSeek) · Mikrobenchmark: CPU na GB przy `write` vs `MSG_ZEROCOPY` vs `io_uring`, kabel 2,5G minipc->serwer,
  ramki 64 KB-8 MB (jądro to 17-20% CPU klienta). · Status: do zmierzenia.
- **A7 — pula buforów zamiast świeżych alokacji w wątku wysyłki** (2026-11-18, Claude, z profilu: `clear_pages` i `rep_movs` w `gpu-icd-send`) · Pomiar: perf przed/po. · Status: do zrobienia.
- **A8 — sortowanie zakresów różnic przed pakowaniem** (2026-11-18, DeepSeek) · Pomiar offline na nagraniu zapisów (rozmiar po kodowaniu, liczba ramek). · Status: do zmierzenia.
- **A9 — VPP na iGPU dla NV12->BGRA** (patrz V1).

## Wyniki zmierzonych eksperymentów

- 2026-11-18, minipc N6000: LZ4 zamiast zstd = +6 FPS w Tomb Raiderze (14-17,5 -> ~20 FPS); surowe zapisy przy pustej kolejce = brak zysku (ponad łącze w ciężkich scenach).
  Szczegóły: `docs/measurements/2026-11-05-rottr/README.md`, Runs 35-41.
