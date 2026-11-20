# Rise of the Tomb Raider (DX11/DXVK, Proton-CachyOS) na desktopie Ryzen 5 1600X, serwer RTX 3090 Ti, wersja 0.3.28 — 2026-11-21

Wbudowany benchmark (Full HD, jak w poprzednich pomiarach), uruchomiony ręcznie z Heroic; zrzut wyniku: `benchmark-result.png`.

| Scena | FPS (min / maks) |
|---|---|
| Mountain Peak | 29,90 (10,61 / 52,84) |
| Syria | 28,62 (5,46 / 50,69) |
| Geothermal Valley | 26,55 (9,91 / 49,81) |
| **Overall** | **28,34** |

Poprzedni pomiar z minipc (2026-11-05): Overall 25–26 (Mountain Peak 38–40, Syria ~18, Geothermal ~19). To INNY komputer klienta, więc nie jest to porównanie jabłek z jabłkami;
bazowy przebieg na tym samym desktopie z wyłączonymi dzisiejszymi opcjami (`GPU_ICD_HOTPUSH=0 GPU_ICD_READCHAN=0 GPU_ICD_LAZY_UNLOCK=0`) — patrz niżej.
Statystyki ICD z przebiegu: gorące okna dołączone do oczekiwań 3,9 tys. okien (6 MB), pobrania leniwe 3 — w DX11/DXVK wzorzec stałych okien odczytu nie występuje, więc nowy odczyt różnicowy
prawie nie działa; oczekiwania na semafor: 28 tys. wraca wcześniej, 4 tys. z odświeżeniem pamięci.

## Baza na tym samym desktopie (opcje wyłączone)

Zrzut: `benchmark-result-baseline-options-off.png`.

| Scena | Opcje domyślne (0.3.28) | Opcje wyłączone |
|---|---|---|
| Mountain Peak | 29,90 | 29,88 |
| Syria | 28,62 | 27,83 |
| Geothermal Valley | 26,55 | 27,69 |
| **Overall** | **28,34** | **28,51** |

**Wniosek: dzisiejsze zmiany (odczyt różnicowy gorących okien, kanał odczytów, pobranie bez blokady) nie zmieniają wyniku Tomb Raidera** (różnica 0,2 FPS, w granicach szumu), co zgadza się ze statystykami
(przy opcjach wyłączonych: 1 774 pobrań leniwych, 110 MB; przy włączonych: 3). Poprawa względem minipc (25–26 → 28,4) pochodzi więc z innego komputera klienta (desktop) i z wcześniejszych prac, a nie z dzisiejszego przełomu,
który dotyczy wzorca „stałe okna odczytu GPU->CPU po każdym oczekiwaniu” (vkd3d/DX12). Oba przebiegi Tomb Raidera szły w pełnym ekranie: gra startuje w oknie, a użytkownik przełączał ją ręcznie skrótem Super+F (pełny ekran Hyprland/Omarchy) przed benchmarkiem.
