# Wykrywanie zapisów do pamięci: `PAGEMAP_SCAN` kontra alternatywy (2026-11-18)

Metodologia: `tools/microbench/scan` (region `mmap` w pełni zapełniony, każda metoda na identycznej sekwencji losowych zbiorów stron, weryfikacja dokładnego zbioru, czas samego skanu).
Komputery: DEV (Ryzen, jądro 7.2.15-omarchy) i minipc (Pentium Silver N6000, to samo jądro). Czas skanu w µs na iterację (średnia).

## minipc N6000 (`SCAN_NOHUGE=1`)

| region MiB / zmienione strony | pmscan (nasza metoda) | soft-dirty | hash CRC32C | memcmp |
|---|---|---|---|---|
| 64 / 20 | **32** | 826 | 13 694 | 8 541 |
| 256 / 200 | **136** | 5 021 | 54 746 | 39 204 |
| 256 / 2000 | **232** | 5 738 | 60 881 | 42 073 |
| 1024 / 200 | **593** | 18 491 | 243 627 | 175 338 |

## DEV (Ryzen)

| region MiB / zmienione strony | pmscan | soft-dirty | hash | memcmp |
|---|---|---|---|---|
| 64 / 20 | **30** | 680 | 11 691 | 5 743 |
| 256 / 200 | **125** | 3 248 | 46 780 | 23 416 |
| 256 / 2000 | **200** | 3 355 | 47 868 | 23 579 |
| 1024 / 200 | **443** | 11 738 | 186 243 | 92 653 |

## Wnioski (zmierzone)

1. **Nasza obecna metoda (userfaultfd async WP + `PAGEMAP_SCAN`) jest 25-30x szybsza od soft-dirty i 300-400x od haszowania, na obu komputerach.** Pomysł "soft-dirty zamiast skanowania"
   (A4) jest zmierzony i gorszy. Pomysł "userfaultfd write-protect" (A5) to dokładnie to, co już robimy.
2. **Przy THP=always (domyślne na obu komputerach) soft-dirty zwraca nadmiarowe strony** (granulacja 2 MiB; weryfikacja zbioru zawodzi już w pierwszej iteracji), więc dla dokładnych zbiorów jest nieprzydatny
   bez `MADV_NOHUGEPAGE`. `PAGEMAP_SCAN` z WP jest dokładny także z THP.
3. **Koszt skanu `PAGEMAP_SCAN` rośnie z rozmiarem obszaru** (32 -> 136 -> 593 µs dla 64 -> 256 -> 1024 MiB na N6000; ok. 0,5 µs/MiB), a słabiej z liczbą zmienionych stron
   (200 -> 2000 stron: +70% na N6000). W grze skan trwa ok. 0,9 ms i jest wykonywany ok. 5 razy na klatkę pod blokadą instancji (Runs 31-32), czyli to ten składnik jest wart optymalizacji:
   **nowy pomysł A10: skanować tylko zakresy, które mogą zawierać zmiany (zamiast całego obszaru), i/lub rzadziej** (patrz `docs/OPTIMIZATION-IDEAS.md`).
