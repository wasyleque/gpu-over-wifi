# Eksperyment: śledzenie zmienionych stron pamięci cieniowej (2026-11-01)

Program: `shadow-tracking-wp.c` (`gcc -O2 -o wp shadow-tracking-wp.c && ./wp 100`). Pamięć: `memfd` MAP_SHARED, 100 MiB (25 600 stron po 4 KiB), po jednym zapisie na stronę. Laptop, jądro 7.2.8, użytkownik bez uprawnień.

| Metoda | założenie ochrony | zapisy (łącznie) | na stronę | zbieranie zmian |
|---|---|---|---|---|
| A. `mprotect` + `SIGSEGV` | 1,88 ms | 152,8 ms | **5,97 µs** | w handlerze (async-signal-safe) |
| B. `userfaultfd(UFFD_USER_MODE_ONLY)` `WP_ASYNC` + `PAGEMAP_SCAN` | 2,12 ms | 45,6 ms | **1,78 µs** | 0,83 ms, 1 zakres |

Kolejny skan bez zapisów: 0 zakresów (ochrona założona ponownie przez skan). B działa bez uprawnień roota (`UFFD_USER_MODE_ONLY`), co obala argument "wymaga unprivileged_userfaultfd" z konsultacji (część 3). Wymaga jądra >= 6.7 (`WP_ASYNC`, `PAGEMAP_SCAN`); dla starszych jąder rezerwą jest haszowanie lub A.
Nie zmierzono jeszcze: haszowania 100 MiB, kosztu przy wielu wątkach piszących, hugepages, pamięci GPU-mapowanej na serwerze.
