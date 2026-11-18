Mikrobenchmark A4/A5 (2026-11-18): cztery metody wykrywania stron zmienionych przez aplikację w dużym obszarze `mmap`:
A `pmscan` (userfaultfd WP_ASYNC + `PAGEMAP_SCAN`, jak `crates/gpu-icd/src/memtrack.rs`), B `softdirty` (`clear_refs` + bit 55 `pagemap`),
C `hash` (hasz CRC32C każdej strony), D `memcmp` (kopia lustrzana). Weryfikuje, że każda metoda zwraca dokładnie zapisany zbiór stron.
Użycie: `scanbench <region_MB> <zmienione_strony_na_iterację> <iteracje> [seed]`; `SCAN_NOHUGE=1` wyłącza strony transparentne (THP).
Kod wygenerowany wspólnie z DeepSeek v4-pro (szkielet i poprawka błędów kompilacji), poprawki: wrapper `mmap`+`madvise`, licznik zapisu 64-bitowy ze stałym znacznikiem.
Wyniki: `docs/measurements/2026-11-18-scan/README.md`.
