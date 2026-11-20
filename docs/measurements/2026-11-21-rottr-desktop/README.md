# Rise of the Tomb Raider (DX11/DXVK, Proton-CachyOS) na desktopie Ryzen 5 1600X, serwer RTX 3090 Ti, wersja 0.3.28 — 2026-11-21

Wbudowany benchmark (Full HD, jak w poprzednich pomiarach), uruchomiony ręcznie z Heroic; zrzut wyniku: `benchmark-result.png`.

| Scena | FPS (min / maks) |
|---|---|
| Mountain Peak | 29,90 (10,61 / 52,84) |
| Syria | 28,62 (5,46 / 50,69) |
| Geothermal Valley | 26,55 (9,91 / 49,81) |
| **Overall** | **28,34** |

Poprzedni pomiar z minipc (2026-11-05): Overall 25–26 (Mountain Peak 38–40, Syria ~18, Geothermal ~19). To INNY komputer klienta, więc nie jest to porównanie jabłek z jabłkami;
bazowy przebieg na tym samym desktopie z wyłączonymi dzisiejszymi opcjami (`GPU_ICD_HOTPUSH=0 GPU_ICD_READCHAN=0 GPU_ICD_LAZY_UNLOCK=0`) jest do zrobienia.
Statystyki ICD z przebiegu: gorące okna dołączone do oczekiwań 3,9 tys. okien (6 MB), pobrania leniwe 3 — w DX11/DXVK wzorzec stałych okien odczytu nie występuje, więc nowy odczyt różnicowy
prawie nie działa; oczekiwania na semafor: 28 tys. wraca wcześniej, 4 tys. z odświeżeniem pamięci.
