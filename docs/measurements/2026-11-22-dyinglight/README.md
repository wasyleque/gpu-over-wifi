# Dying Light (część 1, Epic, Proton/DXVK, DirectX 11) przez zdalny Vulkan

Wersja 0.3.81, klient: desktop Ryzen 5 1600X, 2,5 Gb/s, serwer: RTX 3090 Ti. Gra nie ma wbudowanego benchmarka, więc FPS i płynność liczone są z logu ICD (`opóźnienie klatki`, okna po 5 s) i z licznika DXVK (`DXVK_HUD=fps,frametimes`). Jeden ręczny przebieg po zapisie z samouczka; trasa nie jest powtarzalna co do metra, więc wyniki są orientacyjne.

Ustawienia (zrzuty: `options-video-1.png`, `options-video-2.png`): 1920x1080 pełny ekran, **VSync OFF, limit klatek UNCAPPED** (wcześniejsze przebiegi z `VSync(1)` i limitem dawały sztuczne 30 i 60 FPS: równe 148-150 klatek na 5 s), tekstury/cienie/trawa HIGH, AO, HBAO+, głębia ostrości, ruch rozmycie, AA, ziarno, aberracja chromatyczna ON, PCSS OFF.

| Fragment przebiegu (okna 5 s) | FPS | p50 / p95 / p99 klatki | Przestoje > 100 ms |
|---|---|---|---|
| Wnętrze (okna 6-33, ok. 140 s) | **54-57** (średnio ok. 55,5) | 17,6-18,5 / 26-30 / 30-33 ms | 0 |
| Zewnątrz, dach/droga w samouczku (okna 46-57, ok. 60 s) | **23-39** (średnio ok. 30) | 26-39 / 52-85 / 69-160 ms | 8 z ok. 3 000 klatek |
| Przerywnik (okna 36-41) | równe 30 (limiter gry) | 33,8 / 34,8 / 35-43 ms | 0 |
| Wczytywanie na początku i przy zmianie sceny | spadki do 19-27 | do 760-870 ms | pojedyncze okna z 7-11 klatkami |

Zrzut z zewnątrz: licznik DXVK 28,4 FPS, wykres czasu klatki max 141 ms (`outdoor-tutorial-28fps.png`).

Wnioski: wnętrza przy HIGH działają płynnie (ponad 55 FPS bez przestojów), widok na otwarte miasto z dużym zasięgiem (VisRange 2.0) schodzi do ok. 30 FPS. Nie sprawdziłem jeszcze, ile z tego to GPU serwera, a ile CPU klienta (gra DX11/DXVK jest silnie obciążona po stronie CPU, jak ROTTR w DX11), więc nie wyciągam przyczyny. Natywnego odniesienia na GTX 1080 Ti nie mierzyłem.
