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

## Architektura (ryzykowne, wymagają osobnych dowodów poprawności)

- **A1 — pipeline bez blokady instancji** · wysokie ryzyko poprawności · status: odłożone do czasu danych z profilu blokad.
- **A2 — prefetch / diff deskryptorów** · status: do zmierzenia (liczba chybień cache na grę).
- Odrzucone po analizie (2026-11-18): spekulatywne zwalnianie semaforów (łamie poprawność), userfaultfd-WP (koszt page-fault), MSG_ZEROCOPY (mały zysk),
  soft-dirty (już używamy PAGEMAP_SCAN).

## Wyniki zmierzonych eksperymentów

- 2026-11-18, minipc N6000: LZ4 zamiast zstd = +6 FPS w Tomb Raiderze (14-17,5 -> ~20 FPS); surowe zapisy przy pustej kolejce = brak zysku (ponad łącze w ciężkich scenach).
  Szczegóły: `docs/measurements/2026-11-05-rottr/README.md`, Runs 35-41.
