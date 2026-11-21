# DESKTOP (Ryzen 5 1600X, GTX 1080 Ti, 16 GB) przez 2,5 GbE, Rise of the Tomb Raider, 2026-11-19
Łącze: Intel I226-V → serwer (10.42.0.2), 2,35 Gb/s (skrypt TCP), RTT 0,33 ms. Klient: ICD 0.3.16 (md5 d0a15e14), domyślny kodek zapisów (bez słownika), H.264 dekodowany sprzętowo (NVDEC).

| przebieg | Mountain Peak | Syria | Geothermal | Overall |
|---|---|---|---|---|
| DESKTOP, 2,5 GbE | 58,62 | 32,43 | 31,10 | **41,18** |
| DESKTOP, 1 GbE (wcześniejszy punkt odniesienia) | | | | 30,86 |
| minipc N6000 (LZ4) | | ≈15–16 | ≈14–15 | |

Średnia z logu 41,9 FPS (5863 klatek), na łączu 10,9 GB. Zmiana 1 GbE → 2,5 GbE: +33% Overall. Brak próbników CPU (thrsample/thermlog) na desktopie, więc „CPU klienta na klatkę" jeszcze niezmierzone.

## Cyberpunk 2077 na DESKTOPIE (Ryzen 5 1600X) — to samo zawieszenie co na minipc (N6000)
Hipoteza z 2026-11-18 „klient za słaby" **obalona**: na Ryzenie ten sam objaw (ładowanie benchmarka staje, gra znika po ok. 100 s od startu; log `icd-cp2077.log`, 1682 klatek).
Pierwsza próba: okno 941×1030 (nieparzyste) → koder H.264 odrzucał klatki ("wideo zatrzymane po 0 klatkach"); naprawione pełnym ekranem 1080p w `UserSettings.json`, drugi przebieg ma ten sam objaw co minipc.
Dane z ostatniego okna statystyk:
- obiegi po sieci 110 984, oczekiwanie na odpowiedzi serwera 25,2 s; GameThread 88 s CPU (wątek zajęty, nie śpi);
- deskryptory: lokalnie 689 / cache 0 / serwer 19 764; chybienia wg typu: 17 708 to typ 2 (SAMPLED_IMAGE) — każdy nowy widok obrazu = obieg ~370 µs, cache nie pomaga, bo każdy deskryptor pytany raz;
- GetSemaphoreCounterValue 6423× po 1542 µs (9,9 s), GetShaderModuleIdentifierEXT 3180× po 1401 µs (4,5 s), WaitSemaphores 619× po 2824 µs;
- pod blokadą instancji: settle_memory 33,8 s (PAGEMAP_SCAN 29,8 s w 175 306 wywołaniach, 170 µs), blokada WaitSemaphores trzymana 15 s przy czekaniu innych wątków 18,8 s; skanów 43 801 przy 21 342 QueueSubmit2.
Kandydaci (niezmierzone): (a) spekulacyjny `vkGetDescriptorEXT`/`vkGetShaderModuleIdentifierEXT` dosłany w tej samej partii zaraz po utworzeniu widoku/modułu (bez dodatkowego obiegu); (b) rzadszy skan pod blokadą (skany inne niż po QueueSubmit); (c) lokalna synteza deskryptorów obrazów.

## Cyberpunk 2077 — przebieg do końca (DESKTOP, 2,5 GbE, 1080p, preset Niska, FSR auto, bez RT)
Wbudowany benchmark: **12,54 FPS średnio** (min 8,15, max 18,04), 1001 klatek w 79,81 s. GPU serwera pobiera tylko 111 W z 450 W: wąskim gardłem nie jest GPU ani łącze (na łączu 3,8 GB w całym przebiegu), tylko liczba obiegów i skan pamięci.
W całym przebiegu: `PAGEMAP_SCAN` 606 673 wywołań po 163 µs = 98,6 s (to więcej niż czas benchmarka), 3,6 mln wejść przetwarzania pamięci.

### Przyczyna zawieszeń (stos zdjęty gdb, `stall-cp2077-bt.txt`, `stall-cp2077-bt2.txt`)
Wątek gry (raz redDispatcher8, raz główny GameThread) stoi w jądrze w `handle_userfault`, a `gpu-icd-lazy` siedzi w `poll`; w logu brak porażek `UFFDIO_CONTINUE`.
Zawieszenie ustępuje po dołączeniu gdb (ponowienie błędu strony). Wniosek: **utracone wybudzenie**. Jądro budzi przy `UFFDIO_CONTINUE` tylko strony faktycznie odwzorowane tym wywołaniem; zakres staje na pierwszej stronie już odwzorowanej (EEXIST), więc strona błędu za nią nie jest budzona.
Poprawka (`lazymap::cont`): przy częściowym sukcesie/EEXIST odwzorować resztę strona po stronie i `UFFDIO_WAKE` na cały zakres; porażki innego rodzaju logowane. Biblioteka 9540ca42 wdrożona na desktop; potwierdzenie = kilka przebiegów Cyberpunka bez zawieszenia (jedno nieudowodnione).
Nie wiadomo jeszcze, czy to samo powodowało zawieszenie na minipc (N6000) 2026-11-18.

## POTWIERDZENIE na minipc (N6000): Cyberpunk przeszedł benchmark po poprawce wybudzeń
Biblioteka 9540ca42, przebieg 2026-11-19 22:1x: **9,54 FPS średnio** (min 6,08, max 13,14), 972 klatki w 101,9 s, 1080p, preset Niska, FSR auto, okno bez ramki. Wcześniej (2026-11-18) ten sam test zatrzymywał się w ładowaniu 3 razy na 3 (watchdog gry po 120 s).
W logu 8 porażek `UFFDIO_CONTINUE`: **errno 11 (EAGAIN) po częściowym odwzorowaniu** (np. 4096 lub 28672 z 65536 B). Poprzedni kod ignorował wynik, więc strona błędu zostawała bez wybudzenia i wątek gry stał w `handle_userfault`: **to była przyczyna zawieszenia Cyberpunka na obu klientach, a hipoteza „klient za słaby" była błędna**.
W przebiegu: `PAGEMAP_SCAN` 825 543 wywołań po 125 µs = 103 s, na łączu 3,2 GB. Rise of the Tomb Raider też korzysta z leniwych odwzorowań, więc ten sam błąd mógł być jednorazowym źródłem rzadkich zacięć także tam.

## Płynność (jitter) w Tomb Raiderze, DESKTOP 2,5 GbE, ICD c90a37e7 — Overall 40,94
Nowe statystyki: `odstępy wyświetlenia` i `odstępy oddania` (co 5 s, p50/p95/p99/maks).
- Sceny lekkie: oddanie i wyświetlenie p50 16,7 ms, p95 ≈17 ms (równo, limit 60 FPS).
- **Scena 3 (ciężka)**: oddanie p50 25–32 ms, p95 90–111 ms, p99 110–145 ms; wyświetlenie p50 30–40, p95 54–70, p99 ≈90. Czyli **nierówno wytwarza klatki sama gra** (jej wątek staje na naszych obiegach), a kolejka klatek wygładza to tylko częściowo. Hamowanie gry (limit klatek w locie) w tych oknach ≈0, więc `GPU_ICD_MAX_INFLIGHT` nie jest przyczyną.
- Na klatkę w scenie 3: oczekiwanie na odpowiedzi serwera 9–15 ms, skan pamięci 8–9 ms, 8–10 obiegów (w scenach lekkich: 0,6–3 ms oczekiwania, 1,4–2 ms skanu).
- Obiegi wg poleceń (scena 3, 3 okna ≈15 s): **GetDescriptorEXT 184× = 2973 ms (16 ms/pytanie)**, WaitSemaphores 20× = 1149 ms (57 ms), GetDescriptorSetLayoutSizeEXT 22× = 343 ms. W scenach lekkich: GetDescriptorEXT ≈0,7 ms/pytanie.
- Wniosek (częściowo zmierzony, częściowo hipoteza): pytanie o deskryptor wraca po 16 ms, bo serwer obsługuje żądania po kolei i odpowiada dopiero po zakończeniu wcześniejszej partii (hipoteza: kolejka za pracą GPU); wątek gry czeka na każde nowe, niewidziane wcześniej, widoki obrazów.
Kandydaci: **G1** spekulacyjny deskryptor wysyłany w tej samej partii zaraz po utworzeniu widoku obrazu (odpowiedź trafia do cache zanim gra zapyta; gra nie czeka); **P2** tańszy skan (8–9 ms/klatkę w scenie 3); **G2** bufor wyrównujący odstępy wyświetlania po stronie klienta (kosztuje opóźnienie, wygładza tylko wyświetlanie, nie wytwarzanie).

## Pętla optymalizacji serwera (TR, DESKTOP 2,5 GbE) — pomiary 2026-11-19
Profil perf wątku obsługi `gpu-renderd` w scenie 3 (liczniki GPU_RENDERD_STATS: na klatkę zapisy pamięci 6,7 ms, polecenia 6,5 ms (dekodowanie 49 s z 140 s), zapytania 2,3 ms; 40 mln razy CmdSetDescriptorBufferOffsets2EXT, 14,8 mln CmdBindPipeline na 24,6 mln rysowań; GPU SM 4–14%, wątek obsługi 42→77% jednego rdzenia).

| konfiguracja | Overall | oddanie p95 sc.3 | uwagi |
|---|---|---|---|
| G1 (spekulacyjne deskryptory) | 40,94 | 72–112 ms | 11 816 zapytań w cache; zapytań do serwera nadal ok. 1000/15 s |
| + serwer bez getenv/format!/SipHash | 41,68 | 85–113 ms | |
| + pomiary czasu tylko z STATS (`[vdso]` 8% → 0) | 41,93 | 85–115 ms | profil: crc32c 10,2%, zstd 12,5%, page_hash 8,5% |
| kodowanie LZ4 (`GPU_ICD_MEM_LZ4=1`) | **38,15** | 107–149 ms | na łączu 22,7 GB (×2,1); lz4_flex::decompress_safe = **24,3%** wątku (zstd 12,5%) |

Wniosek: na desktopie z szybkim łączem LZ4 jest GORSZY od zstd+prefiks — dekompresja `lz4_flex` w trybie safe na serwerze kosztuje więcej niż zstd, a bajtów jest dwa razy więcej. Drobne poprawki serwera dały razem ok. +2% FPS; jitter sceny 3 nie zmienił się. Wąskie gardło to praca jednego wątku serwera (crc + dekompresja + hashowanie stron + dekodowanie/odtwarzanie) — kierunek: potok (osobny wątek na odczyt, crc, dekompresję) i mniej powtórek stanu.

## Natywna GTX 1080 Ti vs zdalna RTX 3090 Ti przez LAN (TR, DESKTOP) i dysk
| konfiguracja | Mountain | Syria | Geothermal | Overall |
|---|---|---|---|---|
| natywnie GTX 1080 Ti, 1. przebieg (zimny cache shaderów) | 60,15 | 40,71 | 47,38 | 49,91 |
| natywnie GTX 1080 Ti, 2. przebieg | 59,97 | 59,86 | 60,11 | **59,99** (limit 60 Hz, płynnie) |
| zdalnie RTX 3090 Ti przez 2,5 GbE (nasz ICD) | 59,4 | 33,7 | 31,0 | 41,8 |

Dysk (Micron 2450 NVMe, `tools/iosample.py`): natywnie 87 MB odczytu / 32 MB zapisu / 89 ms wstrzymań I/O na przebieg (125 s); przez ICD 6 MB / 42 MB / 148 ms (151 s), szczyt zapisu 21 MB/s. Hipoteza „więcej operacji dyskowych" obalona pomiarem; brak swapowania (pswpout 198 stron od startu).
Natywnie scena 3 jest płynna (60 FPS stałe), zdalnie szarpie (oddanie p95 85–115 ms przy medianie 25–30 ms). Konsultacja DeepSeek (v4-pro): najlepszy testowalny trop = early-return `vkWaitSemaphores` (gra zaczyna klatkę za wcześnie, potem staje na serwerze); test `GPU_ICD_EARLY_WAIT=0`.

## Cyberpunk 2077: natywnie GTX 1080 Ti vs zdalnie (DESKTOP, 1080p, Niska, FSR auto, bez RT)
Natywnie (1080 Ti, 11 GB): **85,92 FPS** średnio (min 64,76, max 110,61), 5521 klatek w 64,26 s, płynnie.
Zdalnie (RTX 3090 Ti przez 2,5 GbE, ICD 9540ca42): 12,54 FPS (min 8,15, max 18,04), 1001 klatek w 79,8 s. Różnica ok. 7×, czyli Cyberpunk (vkd3d-proton, D3D12) jest dla zdalnego potoku o wiele trudniejszy niż Tomb Raider (natywnie 60 vs zdalnie 42 w DXVK).
Limit klatek `GPU_ICD_PACE_FPS=30` w Tomb Raiderze: Overall 28,74, p95 oddania 34–42 ms przy medianie 33 ms (bez limitu 85–115 przy 25–30), płynność oceniona jako akceptowalna.

## Wpływ debugów na benchmark (Cyberpunk, DESKTOP, ICD 3aa716d2)
Cztery przebiegi tej samej konfiguracji: 13,53 / 13,00 / 13,85 FPS ze `GPU_ICD_STATS=periodic`, `GPU_ICD_LOG` i `GPU_ICD_PTRACE`; **13,76 FPS bez nich** (zostało tylko `GPU_ICD_DESC_PREFETCH`). Rozrzut ±0,4 FPS: wpływ debugów sterowanych zmiennymi nie jest mierzalny. Nie zmierzono jeszcze kosztu liczników wbudowanych na stałe (zegary w ścieżkach polecenia, blokad, skanu): kandydat na osobny pomiar przez flagę kompilacji.
G1 (spekulacyjne deskryptory obrazów) w Cyberpunku: 92 874 zapytań w cache, ale 31 016 z 31 653 chybień typu 2/3 to „ten sam kształt": gra pyta o deskryptor zaraz po utworzeniu widoku, odpowiedź nie zdąży wrócić.

## Cyberpunk 2.5 GbE, biblioteka 1ac1e730 (L1 + SEM_CACHE, G1 wyłączone) – 2026-11-20
Wynik: **16,22 FPS** (min 10,94, maks 22,68, 1116 klatek, 68,8 s); baza L1 15,72. Native 1080 Ti: ~86.
Liczniki z całej sesji (menu + ładowanie + benchmark): pobrania leniwe 142 104 razy, **łącznie 225 s (śr 1,6 ms, 63 KB na pobranie)**, 8,96 GB;
skan pamięci 96 s; oczekiwanie na odpowiedzi serwera 48 s; wysyłka różnic 11,7 s; powody skanu [flush 26 193, leniwy 142 104, sync 101 809].
Wniosek: pobranie leniwe (1,6 ms, ~6× RTT) jest największym kubełkiem, przed skanem. 1,6 ms ≫ RTT 0,25 ms, więc pobranie
czeka głównie w kolejce serwera za nieprzetworzonymi paczkami (do potwierdzenia pomiarem po stronie serwera), nie na łączu.
Konsultacja (DeepSeek v4-pro): kolejność B (skan tylko gorących zakresów) → A (push/wątek odczytu) → C (tokeny deskryptorów); czwarta dźwignia: dwie klatki w locie.

### Kolejka serwera przy pobraniach leniwych (serwer f525c802, ICD 1ac1e730) – 2026-11-20
Cyberpunk: 1084 klatek w 68,9 s (15,7 FPS; powtórka 16,22 → rozrzut ±0,5). Serwer, ~100 tys. odczytów pamięci:
**śr. kolejka 0,22 ms (maks 9,8 ms), śr. wykonanie 4 µs.** Hipoteza „pobranie stoi w kolejce serwera" **odrzucona pomiarem**:
z 1,7 ms na pobranie serwer to ok. 0,22 ms. Reszta jest po stronie klienta: callback `fetch` robi najpierw skan (settle, ~0,7 ms średnio = 96 s / 128 tys.),
potem wysyłkę zaległej paczki i obieg (RTT ~0,25 ms). Wniosek: skan jest składnikiem pobrania, więc redukcja skanu (B) skraca oba kubełki naraz.
Priorytet pobrań w pipeline serwera nie ma sensu; push (A) ukryłby tylko ~0,5 ms obiegu.

### Czysty przebieg benchmarka (tylko benchmark, bez menu/ładowania; tools/cp-bench.sh) – 2026-11-20
15,52 FPS, 1088 klatek w 70,1 s, ICD 25d8e0f0 z GPU_ICD_SCAN_THREADS=4. Skany: obieg synchroniczny 42 635 skanów = **36,4 s (0,85 ms/skan)**, flush 10 873 = 9,3 s, leniwy 59 933 = 3,2 s.
**Korekta wcześniejszych rachunków:** skanów całej areny jest ~39 na klatkę (nie ~10), razem ok. 45,7 s z 70 s przebiegu, jeśli leżą na ścieżce krytycznej wątku gry (do potwierdzenia pomiarem wątkowym). Skan na 4 wątkach **nie przyspieszył** (0,85 ms vs 0,88 ms przy jednym wątku): jądro najpewniej serializuje PAGEMAP_SCAN z WP_MATCHING na jednej areno-VMA, więc skan równoległy odpada jako dźwignia.
"Łączny czas pobrań leniwych" (144 s w 70 s przebiegu) to suma czasu WĄTKÓW, nie czas ścianowy: wcześniejsze wnioski "pobranie leniwe = największy kubełek" były zawyżone sumowaniem współbieżnych wątków.
Wniosek: dźwignią numer jeden jest liczba i obszar skanów całej areny (hot/cold z pułapką synchroniczną wymaga `vm.unprivileged_userfaultfd=1`).

### Model kosztu PAGEMAP_SCAN (test `scan_cost_model`, maszyna deweloperska, bez rywalizacji z wątkami gry) – 2026-11-20
Arena 576 MiB zapełniona: **jeden przedział 296 µs** (2 ns/stronę); 64 MiB jednym przedziałem 32 µs; **64 rozproszone przedziały po 1 MiB (razem 64 MiB) 161 µs**; 1 MiB 2,1 µs;
**co drugi blok zimny (288 przedziałów) 771 µs, czyli GORZEJ niż pełny skan** (stały koszt ok. 2 µs na wywołanie × liczba przedziałów). W grze pełny skan trwa 0,9 ms (3× więcej niż bez rywalizacji).
Skan nie może przejść przez zimny blok jednym wywołaniem: PM_SCAN_CHECK_WPASYNC przerywa skan na stronach bez WP_ASYNC (a bez tej flagi strony poza uffd liczyłyby się jako "zapisane").
Przebieg HOTCOLD=1 (cool_after 400): 447 zimnych bloków, ochłodzeń 11,4 tys., ogrzań 11,0 tys., 1,09 mln stron przez pułapkę, FPS 14,99 (bez zmiany), skan sync 0,9 ms bez zmiany.
Wniosek: pułapka jest poprawna (testy z wyrocznią), ale dźwignia jest słabsza: koszt skanu rośnie z liczbą przedziałów, a bloki zapisywane co 100–500 skanów cykają gorący-zimny (koszt chłodzenia/ogrzania ok. 250 µs).

### Wynik: hot/cold i skan równoległy NIE zmieniają FPS (Cyberpunk, czyste przebiegi cp-bench, ICD b2ece42c) – 2026-11-20
| konfiguracja | średnia FPS | czas skanów sync (s) | skanów sync | uwagi |
|---|---|---|---|---|
| baza, SCANSTAT wyłączony | 14,69 | 45,4 | 40 955 | |
| HOTCOLD=1, cool_after 2000, SCANSTAT wyłączony | 14,72 | 45,5 | 43 721 | take_dirty_all 34,9 s z czego ioctl 32,1 s w 670 tys. wywołań (48 µs) |
| HOTCOLD=1, cool_after 400 (SCANSTAT włączony) | 14,99 | 29,5 | 32 829 | 11,4 tys. ochłodzeń, 1,09 mln stron przez pułapkę |
| 4 wątki skanu (SCAN_THREADS=4) | 15,52 | 36,4 | 42 635 | skan nie przyspieszył |
Rozrzut między przebiegami ok. ±0,5 FPS, więc wszystko mieści się w szumie. Skracanie skanów (o 13–16 s z 45 s) nie przekłada się na FPS, więc skan NIE jest ścieżką krytyczną klatki (albo jest nią tylko częściowo).
Co jeszcze widać: wątki robocze gry (redDispatcher x10) mają po ~120 s CPU każdy, GameThread ~184 s: klient (Ryzen 5 1600X) jest nasycony CPU; blokada instancji: czekanie 304 s sumarycznie (WaitSemaphores trzyma ją 52 s, w tym skan+flush), obiegi: GetDescriptorEXT 29,5 tys. po 1,27 ms = 37,5 s.
Następny krok oparty na dowodach: profil CPU wątku GameThread (perf na desktopie: wymaga `pacman -S perf` i `kernel.perf_event_paranoid=-1`), zanim ruszymy kolejną "rewolucję".
Stan kodu: HOTCOLD i SCAN_THREADS zostają jako opcje (domyślnie wyłączone), testy z wyrocznią (potwierdzone mutacją) chronią poprawność hot/cold dla przyszłych zastosowań (np. gdy skan stanie się ścieżką krytyczną na słabszym kliencie).

## Profil CPU Cyberpunka (perf, 40 s w środku benchmarka, 14,9 FPS) — 2026-11-20

`BENCH_PERF=1 tools/cp-bench.sh perf-baza` (surowe zestawienia: perf-cp-comm.txt, perf-cp-gamethread.txt).
- 11 wątków `redDispatcher` zużywa po ok. 6–7% wszystkich próbek (razem ok. 70%), ale prawie cały ten czas to JEDNA ciasna pętla
  (adresy 0x141969fa0..fca, ~40 bajtów): aktywne czekanie (spin) na pracę. **Nasycenie procesora klienta było fałszywym tropem** — to czekanie, nie praca.
- Nasz ICD to tylko kilka % (gpu-icd-video ok. 3%, frame0/1 ok. 1%, lazy 0,9%).
- `GameThread` (główny wątek) jest zajęty ok. 8% próbek (≈78% czasu), z czego większość to JĄDRO: `getrusage`, `sched_yield`, zegary — pętla czekania
  Wine/gry, a nie obliczenia gry (JIT tylko 1,6%).
- Wniosek: klatka jest ograniczona OPÓŹNIENIEM (gra czeka na wyniki od GPU/ICD), a nie mocą CPU. To wskazuje na obiegi (leniwe pobrania:
  61 232 razy, śr. 2,36 ms, łącznie 144 s sumy wątków; skany synchroniczne 43,9 s; GetDescriptorEXT), czyli na redukcję liczby i czasu obiegów (R1/R3), nie na tańszy skan.

## Odczyt leniwy: rozbicie czasu i eksperymenty (2026-11-20, Cyberpunk, pełny ekran 1920x1080, ok. 15 FPS)

Rozbicie 2,4 ms na pobranie (58 tys. pobrań/przebieg, 3,7 GB): czekanie na blokadę instancji 25 s; praca pod blokadą 115 s, w tym flush 6 s i samo
wywołanie `MemRead` 104 s (1,77 ms). Serwer: kolejka 0,29 ms, wykonanie 9 µs; klient: kolejka+zapis do gniazda 0,9 ms, zapis→odpowiedź 0,9 ms.
Obsługa błędów stron jest szeregowa (jeden wątek `gpu-icd-lazy`): po obsłużeniu błędu czekał już następny w 23% przypadków.

| Eksperyment | Wynik (FPS) | Wniosek |
|---|---|---|
| `GPU_RENDERD_FLUSH_READS` (odpowiedź na odczyt wychodzi od razu) | 15,05 | bez zmiany; opóźnienie nie siedzi w paczkowaniu odpowiedzi serwera |
| `GPU_ICD_LAZY_WINDOW_MIN=64` (okno min. 256 KB) | 11,77 | gorzej: pobrań prawie tyle samo (52 tys.), bajtów 3,5× więcej; odczyty są rozproszone |
| `GPU_ICD_PREFETCH=1` (pobranie gorących okien po unieważnieniu) | 9,17 | wyraźnie gorzej: 124 tys. pobrań z wyprzedzeniem, błędy stron bez zmiany (gra czyta zaraz po oczekiwaniu, przed końcem pobrania) |
| `GPU_ICD_LAZY_XFER_GATE=1` (unieważniaj tylko gdy praca miała zapis transferem, i tylko bufory docelowe kopii) | 15,38 vs baza 15,18 / 14,56 | pobrań −10%, FPS w granicach szumu |

Log pobrań (`GPU_ICD_FETCHLOG`): 57 tys. pobrań to zaledwie 17 różnych okien (pamięć, offset), ~55 pobrań na klatkę, okna po 64 KB. 64% pobrań niesie
zawartość identyczną jak poprzednie pobranie tego okna, ale zawężenie unieważnień do buforów docelowych kopii usuwa tylko ok. 10% pobrań
(pozostałe są zapisywane kopiami, które zapisują te same wartości) — czyli nadmiarowość nie wynika z nadgorliwego unieważniania.

Profil CPU (patrz wyżej): klatka jest ograniczona opóźnieniem obiegów, nie CPU. Pozostaje strukturalna zmiana: serwer dołącza dane gorących okien do odpowiedzi
na oczekiwanie (push przy zakończeniu fence), żeby błąd strony nie wymagał osobnego obiegu. Dodatkowo okno (nie pełny ekran) dało 16,4 FPS (pojedynczy przebieg).
Sporadyczne awarie gry przy starcie (ok. 1 na 4 przebiegi, przyczyna nieustalona; log kończy się na vkd3d-proton SetPageFaultEnablement).

### Górna granica zysku z usunięcia obiegów odczytu leniwego (DIAGNOSTYKA, 2026-11-20)

`GPU_ICD_LAZY_FAKE=1` pomija pobranie (gra widzi stare dane; to nie jest rozwiązanie, tylko pomiar): **31,9 FPS** (min 19,7, maks 46,8) wobec ok. 15 — czyli
obiegi odczytu leniwego SĄ ścieżką krytyczną klatki (ok. 2× FPS do zyskania), w przeciwieństwie do skanu (−13..16 s skanu: 0 FPS).

### BŁĄD KOLORÓW (2026-11-20, zgłoszony przez użytkownika: czerwone logo na kurtce było niebieskie) — NAPRAWIONY w 0.3.21

Przyczyna: `video.rs` po dekodowaniu H.264 (VA-API zwraca już BGRA) przestawiał jeszcze raz R↔B, gdy format nagłówka klatki to Rgba8 — a serwer koduje
wejście NVENC zgodnie z formatem (ABGR dla RGBA), więc kolory po dekoderze są prawdziwe. Efekt: pełna zamiana czerwieni z błękitem przy H.264 (żółta
roślinność wychodziła cyjanowo-biała), przy raw/LZ4 kolory były dobre (potwierdzone przez użytkownika w drugim przebiegu). Błąd wprowadzony w 2527a43 (dedykowane
połączenie wideo); test `swap_tests` pilnuje, że zamianę dostają tylko klatki raw/LZ4 w RGBA. Po poprawce zrzut ekranu z H.264 ma poprawne kolory (potwierdzone przez użytkownika).
Wniosek: pomiary FPS z H.264 nie były dotknięte (zamiana nie zmienia kosztu), ale JAKOŚĆ obrazu była zła w CP — brakowało testu koloru end-to-end (do dodania).

### Gorące okna dołączone do oczekiwań (`GPU_ICD_HOTPUSH=1`, 2026-11-20) — zmierzone, na razie gorzej

Po `Sync` (oczekiwanie) klient dokłada w tym samym obiegu `MemRead` gorących okien leniwych pamięci (te, które aplikacja wcześniej czytała przy błędach stron; wynik wpisywany po
`lazy_invalidate` i tylko gdy nikt nie zgłosił nowej pracy). Wyniki: wszystkie gorące okna przy każdym oczekiwaniu: **8,5 FPS** (209 tys. okien, 14,7 GB, pobrania leniwe −20%);
okna skojarzone z kluczem oczekiwania (uchwyt pierwszego semafora/ogrodzenia): **11,1 FPS** (201 tys. okien, 13,2 GB, pobrania −22%).
Log `GPU_ICD_FETCHLOG` z kluczem oczekiwania: tylko 9–11 różnych kluczy (dwa semafory czasowe, 100016 i 10000d), a okno czytane po jednym z nich w 40–86% przypadków, więc
sam uchwyt semafora nie wskazuje, po którym oczekiwaniu okno będzie czytane (to ta sama liczba oczekiwań co ok. 28 na klatkę, różnią się wartością semafora).
Wniosek: ~55 pobrań na klatkę jest rozproszone między ~28 oczekiwań na klatkę; skuteczny push wymaga klucza dokładniejszego niż uchwyt (np. para uchwyt + wartość
oczekiwana modulo okres klatki albo numer oczekiwania w klatce) albo — przeciwnie — jednej zbiorczej odpowiedzi na końcu klatki. Do zbadania; opcja zostaje wyłączona.

### Pobranie leniwe bez blokady instancji w obiegu po sieci (0.3.23, domyślnie włączone) — mały, powtarzalny zysk

Wysyłka `MemRead` pod blokadą, odbiór odpowiedzi BEZ niej, wpis danych znów pod blokadą (kontrola epoki jak dotąd). Pary naprzemienne (pełny ekran 1920x1080, CP):
z odblokowaniem 15,66 / 15,24 / 15,55 FPS, bez: 14,48 / 15,08 (+ wcześniejsze bazy 14,56–15,18), czyli ok. +0,7 FPS (+5%); czekanie na blokadę
przy pobraniach 25 s → 12 s. Sam obieg nadal ~2,3 ms: czyste 64 KB po łączu 10.42.0.x to 0,50 ms (p99 0,61), więc ok. 1,3 ms to kolejka wysyłki za
wcześniejszymi (kompresowanymi) danymi + serwer. Następny krok do zmierzenia: osobny kanał odczytu z zależnością „po bilecie N strumienia głównego”.

### Sporadyczne awarie startu gry: przyczyna znaleziona i naprawiona (0.3.24)

Log klienta: WATCHDOG „czeka na odpowiedź serwera 8 s” przy starcie, po czym gra się wywala (Proton „Generating error report”). Serwer raportował `CreateDevice 57x 574 s`
(średnio 8–10 s; ostatnie wywołanie ok. 126 s i wynik -4): **wyciek zasobów sterownika między sesjami**. Pomiar na świeżym serwerze (pid, deskryptory, RSS po kolejnych
sesjach): fds 43 → 64 → 85 → 105 → 125 (po ok. 20 na sesję, głównie `/dev/nvidia0`), RSS 587 → 847 → 1031 → 1211 → 1481 MB; `CreateDevice` świeży 0,5 s.
Przyczyny: (1) `destroy_devices` zwalniał pamięć i urządzenie, ale nie obiekty potomne (pipeline'y, obrazy, bufory, semafory itd.), więc sterownik NVIDIA trzymał kontekst;
teraz niszczymy je w kolejności zależności przed `vkDestroyDevice` — fds stabilne (30, `/dev/nvidia0` 12), `CreateDevice` 0,35 s; (2) areny malloc wątków połączeń trzymały zwolnioną pamięć:
`malloc_trim(0)` po sesji — wzrost RSS z ok. 200 MB na sesję do ok. 20 MB. Wcześniejsze „awarie przy starcie” (1 na 4 przebiegi) były skutkiem narastającego czasu `vkCreateDevice` po wielu przebiegach pod rząd.

### Priorytet wątków gry i kolejka przed odczytem (2026-11-20, serwer już zdrowy)

- `GPU_ICD_NICE_SPINNERS=10` (obniżenie priorytetu 11 wątków `redDispatcher`, które kręcą się w pętli oczekiwania): 15,58 / 15,24 FPS wobec 15,61 / 15,11 bez — **brak efektu**;
  nasze wątki pomocnicze nie czekają na procesor. Opcja zostaje wyłączona.
- Kolejka nadawcza przed odczytem leniwym (nowy licznik): średnio **139 KB w 1,6 partiach** (55 tys. odczytów), czyli ok. 0,45 ms czasu na łączu 2,5 GbE zanim żądanie wyjdzie.
  Razem z kolejką serwera (0,29 ms) to ok. 0,75–1 ms z 2,3 ms na pobranie, zależne od danych, które w strumieniu poprzedzają odczyt, ale go nie dotyczą.
- Projekt do zrobienia (duży, ryzykowny; konsultacja DeepSeek v4-pro 2026-11-20): osobne połączenie odczytów (jak istniejące połączenie wideo) z zależnością „po bilecie ostatniego
  oczekiwania na GPU”, a nie „po całym poprzedzającym strumieniu”. Ostrzeżenia z konsultacji: licznik musi rosnąć po ZAKOŃCZENIU żądania (także blokującego oczekiwania), wyścig z późniejszymi
  poleceniami piszącymi do tej samej pamięci (odczyt krótki, ale trzeba pokazać, że semantyka nie jest słabsza niż dziś, gdy MemRead też nie czeka na GPU), brak dostępu wątku odczytu do
  stanu sesji (backend należy do wątku wykonawczego: potrzebny wspólny dostęp do odwzorowań pamięci). Test z konsultacji: zapis 0xAA + oczekiwanie, odczyt z zależnością, natychmiast zapis 0xBB —
  odczyt nie może zwrócić 0xBB ani mieszanki; test zakleszczeń (wiele odczytów + intensywny strumień główny).

### Kanał odczytów (0.3.26, domyślnie włączony) i co naprawdę czeka — 2026-11-20

Drugie połączenie TCP (`AttachReads`, protokół: `AttachReads`, `MemReadAfter { after, memory, offset, len }`), z którego serwer czyta wspólny widok odwzorowanej pamięci
(`readview.rs`: tabela odwzorowań pod `RwLock` — wątek odczytu trzyma blokadę odczytu przez cały czas kopiowania, a wykonawca usuwa wpis przed `vkUnmapMemory`/`vkFreeMemory`;
licznik wykonanych ramek strumienia sterującego, zależność liczona 16-bitowo z zawijaniem). Klient liczy zależność jako późniejszą z: ostatniej ramki `Sync` i ostatniej ramki zapisu tej
pamięci (`Link::read_dep`), więc zapisy aplikacji nigdy nie znikają z odczytu, a odczyt nie czeka na polecenia nowsze niż oczekiwanie na GPU. Odmowa serwera (pamięć jeszcze nie odwzorowana w jego
widoku) wraca do zwykłego odczytu strumieniem sterującym bez wyłączania kanału; błąd transportu wyłącza kanał.
Wynik (pary naprzemienne, pełny ekran): z kanałem 15,67 / 16,12 FPS, bez 15,42 / 15,23 (wcześniej z: 15,48–16,24; bez: 15,0–15,6), czyli ok. +0,5 FPS. Testy: `readview` (czekanie z zawijaniem,
odczyt, usuwanie), `conn` (połączenie odczytów end-to-end z żądaniami z błędami), kodek protokołu.
**Kluczowe odkrycie (statystyki serwera):** kolejka żądania na serwerze 7 µs, sama kopia 1 µs, a **czekanie na zależność średnio 1,3 ms** (do 1,9 ms) — odczyt czeka na to, aż wykonawca
dojdzie do ostatniego oczekiwania na GPU. Czyli ok. 1,3 ms z ok. 2,3 ms na pobranie to realny czas pracy GPU i wykonania poleceń przed oczekiwaniem (28 oczekiwań na klatkę, każde
to łańcuch: wysłanie → wykonanie zgłoszenia → praca GPU → powrót), a nie sieć ani kolejki. Dalszy zysk wymaga skrócenia łańcucha (mniej oczekiwań na klatkę albo wcześniejsze wypychanie pracy), nie przyspieszenia samego odczytu.

### Błędy Xid 32 i restart serwera (2026-11-20)

W dzienniku jądra hub-a co 30–60 min (17:27, 17:54, 18:24, 18:36, 19:20, 20:23, 21:41 czasu hub-a) pojawia się `NVRM: Xid 32` (uszkodzony strumień poleceń GPU) z procesu gpu-renderd, jeszcze
przed zmianami z tego dnia; po nim `vkCreateDevice` wisi ok. 126 s i zwraca -4, a gra się wywala. Serwer po `VK_ERROR_DEVICE_LOST` kończy się po 3 s (kod 75), a `gpu-renderd` działa teraz jako
usługa `systemd --user` (`~/.config/systemd/user/gpu-renderd.service`, `Restart=always`), więc wraca sam. Przyczyna samych Xid 32 (prawdopodobnie błędne dane w strumieniu poleceń) nieustalona — do zbadania.

### PRZEŁOM: różnicowy odczyt gorących okien dołączony do oczekiwania (0.3.27, domyślnie) — 15,5 → 25,2 FPS

Diagnoza, która do tego doprowadziła: klatka CP to łańcuch ok. 28 zależnych punktów synchronizacji (oczekiwanie na GPU → odczyt wyniku przez CPU → następne zgłoszenie), a każdy płacił osobny obieg po dane
odczytu leniwego (ok. 2,3 ms; 28 × 2,3 ms ≈ cały czas klatki). Wykonawca serwera jest zajęty tylko ok. 40% czasu (ok. 24 ms na klatkę), GPU i łącze się nudzą. Wcześniejszy HOTPUSH (pełne `MemRead` okien przy każdym
oczekiwaniu) przegrywał na przepustowości (14 GB); wersja różnicowa dokłada do `Sync` żądanie `MemReadDeltaRanges` gorących okien (≥ 3 prawdziwe błędy stron), serwer porównuje je ze stanem już wysłanym (skróty stron)
i odsyła tylko zmienione strony (64% okien jest identycznych), a klient po `lazy_invalidate` wpisuje je i odwzorowuje w widoku aplikacji (`install_fresh`), więc odczyt jest lokalny. Dane zawsze trafiają do kopii (punkt
odniesienia serwera się przesuwa), a oznaczenie „świeże” tylko gdy odpowiedź kompletna i nikt nie zgłosił nowej pracy. Wyniki: 25,46 / 25,07 / 25,15 FPS (maks 58) wobec 15,50 bez; pobrania leniwe 55 → 29,6 tys.,
dołączone dane 731 MB na przebieg. Obraz poprawny (zrzuty). Oczekiwania z gorącymi oknami blokują (nie wracają wcześniej), ale zastępują oczekiwanie + osobny odczyt jednym obiegiem.

### Co dalej po przełomie (2026-11-21)

- Świeżość okien liczona biletem strumienia (`inv_ticket`: dane z odczytu są świeże, gdy wszystkie dotychczasowe unieważnienia pochodzą z oczekiwań wcześniejszych w strumieniu niż nasze), zamiast „nikt nie zgłosił pracy”:
  pominięte instalacje spadły z 144 tys. (2/3) do 14 tys., pobrania leniwe z ok. 55 tys. do 1,7 tys. na przebieg. FPS pozostał ok. 25–27, więc wąskim gardłem nie jest już odczyt.
- Nowa ścieżka krytyczna: blokada instancji (czekanie 384–408 s, trzymanie 131–138 s w przebiegu; `WaitSemaphores` trzyma ją 0,33 ms × 344 tys.). Pod blokadą w oczekiwaniach: skan pamięci 66–82 s, flush 12–16 s,
  unieważnienie leniwych pamięci 7,5 s, wpisanie i odwzorowanie gorących okien 13 s. Skan hot/cold (25,37) i na 4 wątkach (25,75) wobec bazy 25,85 — nadal neutralne, więc sam skan nie ogranicza FPS
  (11 wątków gry czeka na blokadzie, ale to obciążenie przesuwa się między wątkami). Zostają do zbadania: oczekiwania, które blokują grę (gorące okna wyłączają powrót wcześniejszy), i konkurencja o blokadę.

### Strojenie w trakcie sesji (0.3.29) — bez wiedzy o grze

Ręczne profile na grę nie skalują się, więc ICD stroi się sam: trzy opcje zachowujące poprawność (odczyt różnicowy gorących okien, kanał odczytów, pobranie leniwe bez blokady) są przełączane w locie, a ICD mierzy
FPS z własnych `vkQueuePresentKHR` w oknach 3 s i na przemian porównuje wariant bieżący z przeciwnym (po 25 s rozgrzewki, 2 pary okien na przełącznik, margines 5%, powtórka co 5 min). Jawna zmienna
`GPU_ICD_HOTPUSH`/`GPU_ICD_READCHAN`/`GPU_ICD_LAZY_UNLOCK` przypina przełącznik, `GPU_ICD_TUNER=0` wyłącza strojenie. Cyberpunk: tuner sam zmierzył odczyt różnicowy 31,3 FPS (włączony) wobec 16,1 (wyłączony)
i 36,3 wobec 17,8, i zostawił go włączonego; pozostałe dwie opcje uznał za obojętne/pomocnicze. Koszt strojenia: ok. 25,3 FPS w całym przebiegu wobec ok. 25,8 bez tunera (okna z gorszym wariantem).

### Kto blokuje się w oczekiwaniach i na co czeka odbiór odpowiedzi (2026-11-21, Cyberpunk, 25 FPS)

Blokujące `vkWaitSemaphores` (76,8 tys., średnio 6,6 ms) wykonują WĄTKI POMOCNICZE vkd3d-proton (`vkd3d_fence`, `vkd3d_queue`, `vkd3d-swapchain`), nie wątki gry (`redDispatcher` tylko 134 wywołania):
to one budzą grę po zakończeniu pracy GPU, więc ich opóźnienie to opóźnienie sygnału ogrodzenia widzianego przez grę. Serwer wykonuje zapytania szybko (średnio 57 µs), więc 6,6 ms to nie GPU ani wykonawca.
Odbiór odpowiedzi (490 tys. odbiorów): średnio **1,08 ms czekania na własną kolej** (odpowiedzi czytane ściśle w kolejności biletów, przez wątki, które muszą być obudzone) i 0,32 ms samego odczytu ramki — kolejka biletów ma 3× większy koszt niż sieć.
Prior art (docs/PRIOR-ART-2026-11-21.md): najlepszy kandydat to lokalne repliki stanu fence/semafor/query wypychane razem z sygnałem (Venus „feedback slots”, Wang et al. „shadow resources”).

### Rozbicie blokującego oczekiwania i granice (2026-11-21, Cyberpunk, ok. 25,5 FPS)

Jedna iteracja blokującego `vkWaitSemaphores` (80 tys. razy, 0 timeoutów; wątki vkd3d_fence/queue/swapchain): `exec_begin` pod blokadą **1,06 ms**, odbiór odpowiedzi **5,3 ms**, czekanie na blokadę przy wykończeniu 0,6 ms, wykończenie 0,29 ms.
Przy tym wykonanie wszystkich zapytań na serwerze to średnio 57 µs, a czas „kolejka+zapis do gniazda” żądań z odpowiedzią to 3,4 ms, „od zapisu do odpowiedzi” 2,8 ms: żądanie długo stoi w kolejce nadawczej klienta.
Zmierzone i bez wpływu na FPS (25,0–25,7): `GPU_ICD_FRAMERS=6`, `GPU_ICD_ZSTD_LEVEL=-5`, `GPU_ICD_MEM_LZ4=1` (kolejka+zapis spada 3,36 → 1,78 ms z LZ4, ale ścieżka krytyczna jest gdzie indziej);
dedykowany wątek odbioru odpowiedzi zamiast czytania w kolejności biletów (przepisanie `link.rs`: 25,4 FPS, „czekanie na swoją kolej” 1086 µs to po prostu czas przyjścia odpowiedzi, nie kolejka między wątkami) — cofnięte; obniżenie priorytetu wątków `redDispatcher`.
Diagnostyka górnej granicy `GPU_ICD_DIAG_NOSCAN=1` (pominięcie skanu zapisów aplikacji przy oczekiwaniach; dane aplikacji nie docierają, gra nie kończy benchmarka): `exec_begin` spada z 1,06 ms do 0,16 ms, a czekanie na blokadę instancji ze 384 s do 118 s,
czyli **skan zapisów aplikacji pod globalną blokadą zajmuje ok. 0,9 ms × 80 tys. = 72 s czasu blokady** i to on powoduje czekanie 11 wątków roboczych gry na `EndCommandBuffer`/`ResetCommandPool` (łącznie ok. 384 s). FPS bez skanu niezmierzony (gra pada bez zapisów aplikacji).
Wcześniejsze pomiary hot/cold i skanu na 4 wątkach (skracają skan tylko o ok. 19%) były neutralne, bo nie usuwają go spod blokady.
Kandydat (do zbudowania i zmierzenia): usługa skanowania poza blokadą — jeden wątek skanuje arenę w pętli i numeruje zakończone skany, a oczekiwanie czeka POZA blokadą na skan rozpoczęty po jego zgłoszeniu i pod blokadą tylko zużywa zebrane zakresy (poprawność: zapisy sprzed zgłoszenia pracy są w skanie rozpoczętym później).


## Skan zapisów poza blokadą instancji (prescan) — bez zysku (2026-11-21)

Wątek oczekujący skanuje arenę PRZED wzięciem blokady (`crates/gpu-icd/src/prescan.rs`), pod blokadą tylko zużywa gotowe zakresy; poprawność chroni numer zgłoszeń (`SUBMIT_SEQ`) i wspólny mutex zakresów (zakresy zabrane jądru nie giną). Pomiar A/B (tools/ab-run.sh, pełny ekran, ten sam przebieg po kolei):

| wariant | średnia FPS | min | czas skanów pod blokadą (obieg synchroniczny) |
|---|---|---|---|
| prescan włączony | 25,00 | 13,4 | 43,8 s (z 82,8 s) |
| prescan wyłączony | 25,70 | 14,4 | 82,8 s |

Wniosek: skan pod globalną blokadą NIE jest ścieżką krytyczną (czas pod blokadą spadł o połowę, FPS bez zmian). Odrzucone po pomiarze; kod zostaje jako opcja `GPU_ICD_PRESCAN=1`. Pierwszy przebieg z diagnostycznym kluczem NOSCAN w konfiguracji przekroczył limit (gra nie kończy benchmarku bez skanu) — klucze diagnostyczne trzeba usuwać z konfiguracji gry.

## Prefetch deskryptorów obrazów na bazie 25 FPS — bez efektu (2026-11-21)

`GPU_ICD_DESC_PREFETCH=1` (spekulacyjne pytania o deskryptory przy vkCreateImageView) A/B/A: 25,75 / 25,42 (baza) / 25,43 FPS. Różnica w szumie, odrzucone. 37 tys. obiegów GetDescriptorEXT po ok. 4 ms (głównie typ 2) nie leży na ścieżce krytycznej klatki albo prefetch ich nie trafia.

## Wątki `redDispatcher` i priorytety na bazie 25 FPS (2026-11-21)

Profil: `top -H` w trakcie benchmarku — CPU klienta zajęty w 82,5% (user), 6% bezczynności, load 12,75 na 12 wątkach; 11 wątków `redDispatcher` po 70–83%, `perf record` na jednym: ok. 75% próbek w jednej ciasnej pętli (11 adresów w 42 bajtach modułu gry) — aktywne czekanie. A/B po restarcie serwera: baza 25,81 FPS; `GPU_ICD_NICE_SPINNERS=19` 25,14; SCHED_IDLE (`=20`) 24,84. Obniżanie priorytetu spinnerów NIE pomaga (lekko szkodzi), więc CPU klienta nie jest wąskim gardłem przez ich pętle; odrzucone. Zostaje opcją diagnostyczną (wartość 20 = SCHED_IDLE).

Uwaga operacyjna: po wielu ubitych sesjach gra przestawała się uruchamiać (zawieszona na ekranie startowym, trzy próby z rzędu) — pomógł restart `gpu-renderd` (pamięć GPU 12 GB → 0). `tools/ab-run.sh` dostał pełne czyszczenie pozostałości (gogdl, umu_run, REDprelauncher).

## Budżet czasu klatki wg liczników serwera (2026-11-21, baza 25,5 FPS)

Skumulowane liczniki `GPU_RENDERD_STATS` (33 tys. klatek): na klatkę wątek obsługi żądań spędza: polecenia (`cmds`, w tym dekodowanie ok. 1 ms i replay) 2,7 ms, zapisy pamięci 1,6 ms, odczyty różnicowe (`memreaddelta`, ok. 95 µs na żądanie) 3,1 ms, zapytania 0,16 ms, odczyt klatki do kodowania 0,56 ms — razem ok. 8 ms. Kodowanie i wysyłka klatki (`present_encode` 2,3 ms, `present_send` 2,1 ms na klatkę) idą w osobnym wątku (EncJob), poza ścieżką krytyczną. Sieć: ok. 28 obiegów × RTT ok. 0,4 ms = ok. 11 ms. Razem serwer + sieć ok. 19 ms z ok. 39 ms klatki; reszta (ok. 20 ms) to czas po stronie klienta (wątki gry, kodowanie/skan/wysyłka w ICD) i wzajemne czekanie. Bez GPU lokalnego na desktopie nie da się zmierzyć, ile z tego byłoby i tak natywnym kosztem CPU gry (Ryzen 5 1600X), więc sufit remotingu jest dla tej gry niewiadomy; kierunki, które jeszcze zmniejszają czas serwera: równoległe nagrywanie CB (T9), tańszy `memreaddelta` (hash okien), mniej obiegów (lokalna replika semaforów/fence).
Zdarzenie: serwer wyszedł z kodem 75 (VK_ERROR_DEVICE_LOST) o 10:02 podczas testów; supervisor go podniósł, ale gra przy kolejnych startach wisiała aż do ręcznego restartu usługi (do zbadania: odtworzenie po DEVICE_LOST powinno być samoleczące).

## Samoleczenie po DEVICE_LOST: sprawdzenie karty przy starcie demona (2026-11-21)

Zdarzenie 10:02: serwer wyszedł z kodem 75 po `VK_ERROR_DEVICE_LOST`, systemd podniósł go po 1 s, ale kolejne starty gry wisiały na ekranie startowym (`vkCreateDevice` zwracało -3), aż do ręcznego restartu usługi. Przyczyna: sterownik NVIDIA po Xid 32 jeszcze przez jakiś czas nie zakłada urządzeń, a demon mimo to przyjmował sesje.
Poprawka (`crates/gpu-renderd/src/vk/health.rs`): przed nasłuchiwaniem demon zakłada i niszczy jedno urządzenie na każdym widocznym urządzeniu fizycznym (limit 25 s); przy błędzie lub zawieszeniu kończy się kodem 75, więc klient dostaje odmowę połączenia zamiast zawieszenia, a nadzorca (RestartSec zwiększone z 1 do 5 s) ponawia próbę aż karta wróci. `GPU_RENDERD_PROBE=0` wyłącza. Po wdrożeniu: sprawdzenie przechodzi na zdrowej karcie, Cyberpunk bez regresji (25,56 FPS, min 14,1, maks 55,9). Ścieżka awaryjna nie była wywołana na prawdziwej usterce (nie da się jej wywołać na żądanie), więc pozostaje niesprawdzona w praktyce.
