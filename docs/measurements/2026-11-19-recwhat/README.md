# C3b / C5 / C6 / C7 na prawdziwym nagraniu zapisów (Rise of the Tomb Raider, 2026-11-19)

Nagranie: `GPU_ICD_REC_DIR` na minipc (N6000), cztery największe pamięci host-cached (po 512 MB), pierwsze 60 000 rekordów każdej (20% to rozgrzewka lustra/słownika).
Analiza: `tools/microbench/recwhat` uruchomione na minipc (czasy to CPU N6000, procesor bezczynny w 99%).

| pamięć | rekordy | surowo MB | LZ4 MB (%) | zstd1+prefiks MB (%) | słownik 64K MB (%) | zmiany+LZ4 (G=4) MB (%) | cache hash + LZ4 MB | trafienia C5 (bajty) | rewind K=8 |
|---|---|---|---|---|---|---|---|---|---|
| 0x10004a | 48 000 | 305,0 | 79,5 (26,1) | 13,6 (4,5) | 26,2 (8,6) | 17,4 (5,7) | 88,8 | 2,9% | 1,5% |
| 0x100312 | 40 541 | 426,8 | 136,3 (31,9) | 52,8 (12,4) | 66,0 (15,5) | 71,7 (16,8) | 133,3 | 14,7% | 5,5% |
| 0x10095d | 48 000 | 371,9 | 130,5 (35,1) | 33,7 (9,1) | 63,5 (17,1) | 47,8 (12,9) | 125,6 | 19,8% | 3,3% |
| 0x1028fc | 31 634 | 424,8 | 166,9 (39,3) | 70,4 (16,6) | 107,0 (25,2) | 113,8 (26,8) | 122,8 | 32,0% | 3,1% |
| **razem** | | **1528,5** | **513,2 (33,6)** | **170,5 (11,1)** | **262,7 (17,2)** | **250,7 (16,4)** | **470,5 (30,8)** | | |

Czas kodowania (ms, N6000): LZ4 2 491; słownik 64K 3 540 (+42%); zstd1+prefiks 4 294 (+72%); zstd bez słownika i prefiksu ok. 5 400. Wersja `SPARSE` w narzędziu jest naiwna (porównanie bajt po bajcie, 4-8 ms/MB),
więc jej czas nie jest miarodajny dla wdrożenia z SIMD.

## Wnioski (zmierzone)

1. **C7 (słownik zstd wytrenowany na wczesnych zapisach gry, bez lustra) daje o połowę mniej bajtów niż LZ4 przy +42% CPU kodowania.** Nie potrzebuje lustra po obu stronach (które na minipc psuło wyniki zstd z prefiksem).
2. **C5 (cache zawartości po hashu) w obrębie jednej sesji oszczędza tylko 3-26% względem LZ4**, trafienia 3-32% bajtów (jedna pamięć 32%). **C6 ("rewind", 8 wersji): trafienia 1,5-5,5%**, nieopłacalne w tej grze.
   Nie zmierzono jeszcze C5 między sesjami i klientami (kluczowe dla współdzielenia GPU).
3. Dobór kodeka do klienta: słaby CPU + szybkie łącze -> LZ4; słaby CPU + wolne łącze (1 GbE) -> słownik (C7); mocny CPU + wolne łącze -> zstd z prefiksem. Dane do profilu klienta (R1).
