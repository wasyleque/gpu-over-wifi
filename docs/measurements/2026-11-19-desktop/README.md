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

## Profil serwera: indeks rodzajów w rejestrze uchwytów (2026-11-21) — pierwszy zysk po stronie serwera

`perf record -g` na `gpu-renderd` w trakcie benchmarku Cyberpunka: 9% próbek to jedna funkcja — `Registry::entries_of_kind`, która przy każdym odczycie pamięci (`copy_many`, `mem_is_host_cached`) przeglądała cały rejestr uchwytów (setki tysięcy wpisów) tylko po to, by znaleźć urządzenie fizyczne danego `VkDevice`. Poprawka: indeks rodzaj → identyfikatory prowadzony przy `register`/`unregister` (test jednostkowy). Pomiar po wdrożeniu: **26,51 i 26,64 FPS** (min 14,6 / 15,2) wobec 25,4–25,8 przed zmianą, czyli ok. +1 FPS (+4%); żaden wcześniejszy przebieg tego dnia nie przekroczył 25,8. Dalsze pozycje profilu serwera: dekompresja zstd 8%, `page_hash` 4%, CRC32C 3,9%.

## Profil inkluzywny serwera (dwarf): `available_parallelism` przy każdym odczycie (2026-11-21)

`perf record --call-graph dwarf` na `gpu-renderd` w trakcie benchmarku, ranking po czasie inkluzywnym wątku obsługi: `std::thread::available_parallelism` 5,6% (z czego open/read/statx/close plików cgroup), bo `pagediff::page_hashes` wołało je PRZED tanim warunkiem rozmiaru, czyli przy każdym odczycie różnicowym małej pamięci (ok. 1 mln razy na sesję). Poprawka: warunek rozmiaru najpierw, liczba procesorów w `OnceLock` (też w `gpu-vaapi/nv12.rs`, po stronie klienta, raz na klatkę). Pomiar: **28,09 i 27,70 FPS** (min 15,8 / 15,6) wobec 26,5–26,6 po poprzedniej poprawce i 25,4–25,8 przed nią. Reszta profilu wątku obsługi: dekompresja zstd z prefiksem (zależna od lustra pamięci, nie da się przenieść do wątku odczytu) ok. 13%, `page_hash` 4,6%, CRC32C 4,2%.

## Kodek zapisów pamięci wg prędkości łącza: LZ4 na łączach >= 2 Gb/s (2026-11-21)

Zapisy pamięci z prefiksem z lustra (zstd) rozpakowuje wątek WYKONUJĄCY serwera (zależą od stanu lustra, więc wątek odczytu nie może tego zrobić): w profilu dwarf `ZSTD_decompressSequences` ok. 14% próbek. LZ4 i zwykły zstd rozpakowuje wątek odczytu połączenia, poza ścieżką krytyczną. A/B (desktop 2,5 Gb/s): LZ4 **29,11 / 29,46 FPS** (min 17,0 / 16,9) wobec zstd-prefiks 28,31 (min 16,1); po wdrożeniu wykrywania łącza: auto 29,37 / 29,43, wymuszony `GPU_ICD_MEM_LZ4=0` 28,32.
Mechanizm: bez jawnej zmiennej ICD czyta interfejs trasy do `GPU_ICD_ADDR` z `/proc/net/route` (najdłuższy prefiks) i jego prędkość z `/sys/class/net/<if>/speed`; >= 2000 Mb/s -> LZ4, wolniejsze łącza i Wi-Fi (brak prędkości) -> zstd z prefiksem, który wysyła o połowę mniej bajtów (na 1 Gb/s lub Wi-Fi to łącze, nie CPU serwera, jest ograniczeniem). `GPU_ICD_MEM_LZ4=1/0` wymusza. Nie wprowadzono przełączania w locie przez tuner: lustro klienta aktualizują tylko zadania w trybie prefiksu, więc powrót z LZ4 do zstd w trakcie sesji rozjechałby lustra.

## RTT pod obciążeniem gry i korekta budżetu klatki (2026-11-21)

Ping desktop -> serwer w trakcie benchmarku Cyberpunka (200 pakietów co 20 ms): **średnio 0,163 ms** (min 0,076, maks 1,06). Ping bezczynny dawał średnio 0,357 ms (min 0,12) — wydłużenie to wybudzanie CPU ze stanów uśpienia, którego pod obciążeniem nie ma. Obie karty (`igc`) mają łączenie przerwań `rx-usecs 3`, MTU 1500. Wniosek: koszt sieci to ok. 28 obiegów x 0,16 ms = ok. 4,6 ms na klatkę, NIE ok. 11 ms (wcześniejszy budżet zakładał 0,4 ms z pomiaru bezczynnego). Czy strojenie stanów C / łączenia przerwań / busy-poll coś da, NIE jest zmierzone: górna granica to różnica średniej i minimum (0,163 - 0,076 ms) x 28 obiegów = ok. 2,5 ms na klatkę (do ok. 2 FPS); eksperyment wykonany i bez efektu (patrz niżej). Klatka przy 29 FPS to ok. 34 ms: serwer (wątek obsługi) ok. 7 ms, sieć ok. 4,6 ms, reszta ok. 22 ms po stronie klienta i oczekiwania na GPU; ta sama gra z lokalną kartą daje ok. 86 FPS (11,6 ms/klatkę), więc koszt CPU samej gry jest tego rzędu.

### Eksperyment do wykonania (wymaga roota, jeszcze niewykonany)
Na OBU maszynach wyłączyć głębokie stany C i włączyć busy-poll, potem A/B `tools/ab-run.sh` (przed/po):
`sudo sh -c 'for f in /sys/devices/system/cpu/cpu*/cpuidle/state[2-9]/disable; do echo 1 > $f; done; sysctl -w net.core.busy_poll=50 net.core.busy_read=50'`
Wycofanie: `sudo sh -c 'for f in /sys/devices/system/cpu/cpu*/cpuidle/state[2-9]/disable; do echo 0 > $f; done; sysctl -w net.core.busy_poll=0 net.core.busy_read=0'`.

### Wynik eksperymentu stanów C + busy-poll (2026-11-21): bez efektu
Po `tools/cstates.sh on` na obu maszynach (głębokie stany C wyłączone, `net.core.busy_poll=busy_read=50`): **29,49 i 29,04 FPS** (min 17,3 / 17,0) wobec 29,37 / 29,43 (min 17,1 / 17,3) bez zmian; RTT pod obciążeniem 0,152 ms (min 0,061, maks 0,66) wobec 0,163 ms. Różnica w szumie, więc strojenie opóźnień sieci/CPU jest odrzucone (po pomiarze); przywrócenie: `tools/cstates.sh off` (oszczędza prąd w spoczynku).

## Zajętość wątków pomocniczych vkd3d blokującymi oczekiwaniami (2026-11-21, z logu ICD)

Przyrosty liczników `blokujące oczekiwania wg wątku` między kolejnymi liniami logu (odstęp ok. 5 s, ok. 150 klatek, 29 FPS): `vkd3d_fence` #1 +640 oczekiwań i +1,8 s zablokowania (ok. 36% czasu), `vkd3d_fence` #2 +600 i +1,7 s (ok. 34%), `vkd3d_queue` #1 +480 i +1,0 s (ok. 20%), `vkd3d_queue` #2 +320 i +1,0 s (ok. 20%); razem ok. 15 blokujących oczekiwań na klatkę, średnio 4-5 ms każde. Żaden pojedynczy wątek nie jest zablokowany przez cały czas, więc ścieżka krytyczna nie jest jednym wątkiem oczekującym, tylko łańcuchem zależności między kilkoma (fence -> queue -> swapchain -> wątek gry). Opóźnienie klatki od oddania do wyświetlenia to tylko 13-15 ms (93-100% poniżej 20 ms), czyli ok. 20 ms klatki przy 29 FPS to czas PRZED oddaniem (gra + oczekiwania), a nie transport obrazu. Kolejny krok: czasy zdarzeń w obrębie jednej klatki (kto na kogo czeka), żeby wskazać, które oczekiwanie jest ostatnim ogniwem łańcucha.

## Oś czasu klatki: kto na co czeka (GPU_ICD_TIMELINE, 2026-11-21)

Nowe narzędzie diagnostyczne `GPU_ICD_TIMELINE=<s>` (moduł `timeline.rs`): okno 250 ms po `<s>` sekundach, każde wywołanie ICD z czasem trwania, wątkiem i czasem oczekiwania na blokadę instancji, zrzut raz do logu (`[tl]`). Dwa przebiegi w trakcie benchmarku Cyberpunka (29,7 i 29,5 FPS), ok. 830 zdarzeń na ok. 8 klatek:

| wątek / polecenie | wywołań | suma czasu | w tym czekanie na blokadę |
|---|---|---|---|
| vkd3d_fence `vkWaitSemaphores` | 256 | 510 ms | 121 ms |
| vkd3d_queue `vkWaitSemaphores` | 72 | 271 ms | 20 ms |
| redDispatcher `vkGetDescriptorEXT` | 55 | 170 ms | 11 ms |
| redDispatcher `vkResetCommandPool` | 77 | 108 ms | **92 ms** |
| vkd3d_queue `vkQueueSubmit2` | 141 | 101 ms | **79 ms** |
| redDispatcher `vkEndCommandBuffer` | 67 | 94 ms | **85 ms** |
| redDispatcher `vkGetDeviceImageMemoryRequirements` | 25 | 44 ms | 36 ms |

Wnioski: (1) `vkResetCommandPool`, `vkEndCommandBuffer`, `vkQueueSubmit2` to w ok. 80-90% czas czekania na globalną blokadę instancji (a nie praca ICD): sumarycznie 36% czasu wszystkich wywołań w oknie to czekanie na blokadę. (2) `vkGetDescriptorEXT` prawie nie czeka na blokadę (11 z 170 ms): jego ok. 3 ms to obieg po sieci i kolejka na serwerze, a nie ICD; wątek `redDispatcher6` spędza w wywołaniach ICD prawie 100% okna, szeregowo (GetDescriptorEXT, CreateImage, GetImageMemoryRequirements, BindImageMemory2, CreateImageView: tworzenie zasobów). (3) Wcześniejszy eksperyment (prescan, skan pod blokadą 82 -> 44 s) nie podniósł FPS, więc samo zmniejszenie zajętości blokady nie wystarcza; ścieżka krytyczna zależy od łańcucha serwer-GPU, nie tylko od blokady. Kierunki wymagające większych zmian: blokada per kolejka/obiekt zamiast globalnej, zapytania (deskryptory, wymagania pamięci) na osobnym połączeniu wykonywanym równolegle na serwerze, równoległe nagrywanie CB na serwerze.

## Długość odcinka blokującego oczekiwania (head-of-line na serwerze) — bez efektu (2026-11-21)

Hipoteza: blokujące oczekiwanie (odcinek 1 -> 4 ms) zatrzymuje wątek wykonujący serwera, więc zapytania (np. `vkGetDescriptorEXT` po ok. 3 ms) czekają za nim. Test przez nowe `GPU_ICD_WAIT_SLICE_US` / `GPU_ICD_WAIT_SLICE_MAX_US` (domyślnie 1000/4000 µs): 250/1000 -> **29,29 FPS**, 500/2000 -> 28,93, 2000/8000 -> 29,45, baza 1000/4000 -> 28,88 (minima 16,5-17,2). Wszystko w szumie (±0,3), więc długość odcinka nie ma znaczenia i kolejkowanie zapytań za oczekiwaniami nie jest głównym źródłem ok. 3 ms `vkGetDescriptorEXT`. Odrzucone po pomiarze; zmienne zostają jako opcje diagnostyczne.

## Natychmiastowe odpowiedzi serwera (GPU_RENDERD_FLUSH_ALL) — +9% FPS, wyższe minimum (2026-11-21)

Oś czasu ICD (`GPU_ICD_TIMELINE`, z rozbiciem na pierwsze wzięcie blokady / oczekiwanie na odpowiedź / ponowne wzięcie blokady) pokazała dla `vkGetDescriptorEXT` (46 wywołań, śr. 4,0 ms): odbiór odpowiedzi 2,15 ms, ponowne wzięcie blokady po odpowiedzi 0,86 ms, pierwsza blokada 0,27 ms; serwer wykonuje to w 6 µs, RTT 0,16 ms. Przyczyna ok. 2 ms: serwer zbierał odpowiedzi w buforze (`flush_pending`) i wypisywał je dopiero po opróżnieniu CAŁEJ kolejki gotowych żądań, więc odpowiedź na krótkie zapytanie czekała za kolejnymi żądaniami (w tym blokującymi oczekiwaniami na GPU). To było wcześniejsze usprawnienie "mniej pakietów", które przy tym profilu kosztowało opóźnienie.
Poprawka: każda odpowiedź wychodzi od razu po obsłużeniu żądania (`GPU_RENDERD_FLUSH_ALL`, teraz domyślnie włączone; `=0` wraca do zbierania). Pomiar (desktop 2,5 Gb/s): **32,08 / 32,09 / 31,84 FPS, minimum 19,6 / 19,4 / 19,2** wobec 29,10 / 28,24 FPS i minimum 16,8 / 16,5 bez poprawki (ten sam dzień, ta sama konfiguracja). Wcześniejszy test `GPU_RENDERD_FLUSH_READS` (natychmiast tylko odpowiedzi na odczyty pamięci) był neutralny; zysk daje wypuszczanie wszystkich odpowiedzi, w szczególności na zapytania (`vkGetDescriptorEXT`, wymagania pamięci) i oczekiwania.
Pomysł: autor sesji, z lektury `conn.rs` po pomiarze osi czasu; konsultacja z DeepSeek i modelami z .11 dała kierunek pomiaru (znaczniki czasu, hipoteza kolejkowania na serwerze), nie to rozwiązanie.

## Próg wysyłki partii poleceń klienta — bez zysku (2026-11-21)

`GPU_ICD_BATCH_FLUSH_KB` (nowa opcja; mniejszy próg pozwala serwerowi zacząć odtwarzanie, gdy klient jeszcze nagrywa), po restarcie serwera i z już działającym FLUSH_ALL: 8 KB **32,07 FPS**, 16 KB 31,59, 32 KB 31,79, domyślne 64 KB **32,90** (minima 19,7 / 19,7 / 19,6 / 20,5). Mniejsze progi nie pomagają (lekko szkodzą: więcej pakietów), domyślne zostaje 64 KB. Odrzucone po pomiarze. Uwaga: wynik 64 KB (32,9) jest o ok. 0,8 FPS wyższy niż trzy poprzednie przebiegi tej konfiguracji (32,1 / 32,1 / 31,8), co może wynikać ze świeżego restartu serwera — to tylko hipoteza, nie ustalono przyczyny różnicy.

## Dysk serwera (SATA SSD przez USB) — bez wpływu na wynik (2026-11-21)

Serwer ma system, katalog roboczy demona, logi i cache sterownika NVIDIA (131 MB) na dysku USB (`sda`, ext4); szybki NVMe (`nvme0n1`) służy tylko modelom AI (`/srv/models`). `iostat -dxy 5` przez cały przebieg Cyberpunka (start gry + benchmark, 64 próbki po 5 s, 31,98 FPS): `sda` zajęty średnio 0,2% (maks. 1,7%), **zero odczytów** (wszystko z pamięci podręcznej), do 24 zapisów/s i 3,3 MB/s (logi statystyk), maks. czas zapisu 50 ms (zapisy buforowane, nie blokują demona); `nvme0n1` bez ruchu. Wniosek: w stanie ustalonym dysk nie wpływa na FPS. Niezbadane: zimny start po restarcie maszyny (odczyt bibliotek i pierwsza kompilacja potoków z cache na USB) i ewentualne zawieszenie `eprintln` demona, gdyby zapis logu na USB się zatkał.

## Cache sterownika i log serwera przeniesione na NVMe (2026-11-21)

Na prośbę o przeniesienie z dysku USB (`sda`) na NVMe (`/srv/models/gpu-hub/`). **Metoda 1 (nieudana, wycofana):** `__GL_SHADER_DISK_CACHE_PATH=/srv/models/gpu-hub/cache` + `__GL_SHADER_DISK_CACHE_SKIP_CLEANUP=1` w jednostce usługi i log w nowej ścieżce. Skutek: sterownik przyciął skopiowany cache ze 131 MB do 1,3 MB, a dwa starty Cyberpunka z rzędu kończyły się bez wyniku; po powrocie do poprzedniej jednostki gra znów działała (32,23 FPS). Przyczyna niezbadana (podejrzenie: zmiana ścieżki unieważnia cache i potoki kompilują się od zera, albo zmienna zmienia zachowanie sterownika); nie ma pewności, że winna jest sama zmienna.
**Metoda 2 (działa, obowiązuje):** katalog `~/.cache/nvidia` przeniesiony z zawartością do `/srv/models/gpu-hub/cache-nvidia` i zastąpiony dowiązaniem symbolicznym, `~/gpu-hub/gpu-renderd.log` to dowiązanie do `/srv/models/gpu-hub/log/gpu-renderd.log`; jednostka usługi bez zmian względem oryginału (kopie: `gpu-renderd.service.bak-usb`, `.nvme-env`; stary log: `gpu-renderd.log.usb`). Cache nietknięty (131 MB). Dwa przebiegi po zmianie: **32,53 i 32,85 FPS** (minimum 20,0 / 20,9), czyli bez straty i bez mierzalnego zysku, zgodnie z pomiarem dysku (w stanie ustalonym dysk nie obciążony). Wycofanie: `rm ~/.cache/nvidia && mv /srv/models/gpu-hub/cache-nvidia ~/.cache/nvidia`, analogicznie log.

### Korekta: wolne starty gry NIE zależą od liczby sesji (2026-11-21)
Wcześniej (w tym w komentarzach do testu progu partii) wiązałem start gry trwający 110-172 s z nagromadzonymi sesjami na serwerze; to było niesprawdzone. Dane ze wszystkich ok. 30 dzisiejszych przebiegów (czas "gra wystartowała po" z logów `tools/ab-run.sh`): 28 startów po 20-26 s, dwa wyjątki (110 s i 172 s), oba tuż po przerwanym lub zabitym przebiegu (nieudana próba, ręczne zabicie gry, restart serwera w trakcie). Po stronie serwera nic nie rośnie: po kilku sesjach od restartu 20 deskryptorów `/dev/nvidia*` (stan bazowy: instancja trzymająca), RSS 297 MB, 3 wątki. Najbardziej prawdopodobne wyjaśnienie: pozostałości po zabitej grze (Wine, Heroic, sesja u serwera w trakcie sprzątania) opóźniają kolejny start; nie zbadano. Wniosek praktyczny: po zabiciu gry odczekać, a `tools/ab-run.sh` już sprząta pozostałości przed każdym startem.

## Crash Heroic przy każdym przebiegu: przyczyna po naszej stronie (2026-11-21)

Zgłoszenie: przy każdym starcie testu Cyberpunka pojawia się błąd Heroic. Dziennik systemowy desktopa: 11 zrzutów rdzenia `/opt/Heroic/heroic` (SIGTRAP, `trap int3`) tego dnia, co ok. 5 minut, każdy ok. 6 s po zabiciu Heroic przez `pkill -x heroic` (SIGTERM) w `tools/cp-bench.sh`, `tools/ab-run.sh` i `tools/tr-bench.sh`. Eksperyment: `kill -INT` na działającym Heroic zamyka go bez crasha (brak wpisu w `journalctl` i `coredumpctl`). Poprawka: wszystkie trzy skrypty zamykają Heroic przez SIGINT i dopiero po 8 s awaryjnie SIGTERM. Przebieg kontrolny po poprawce (start i zamknięcie Heroic): brak nowego zrzutu; 31,74 FPS.

## Xid 32 na serwerze: dziennik kernela (2026-11-21)

`journalctl -k` na serwerze: Xid 32 (uszkodzony strumień poleceń) od `gpu-renderd` o 09:02, 10:00, 11:42, 15:13, 15:15, 16:54 (ten sam kanał 0x01000009 w większości), Xid 31 (błąd stron GPU, engine CE2) o 12:32, wcześniej Xid 109 (CTX SWITCH TIMEOUT) o 07:50-08:15. Po każdym z nich serwer kończył się kodem 75 (`DEVICE_LOST`) w ciągu ok. 2 minut i przebieg testu padał (o 16:56 gra czekała na odpowiedź na `vkGetPhysicalDeviceImageFormatProperties2` i wywróciła się). O 16:53:33 jeden Xid 32 został przypisany wątkowi `gpu-probe` (nasze sprawdzenie karty przy starcie) — czy to ono go wywołuje, jest niesprawdzone (pozostałe restarty bez Xid przy starcie). Przyczyna Xid 32 w ogóle: niezbadana; ok. 1 na 1-2 godziny pracy.

## Sonda karty przy starcie sama wywoływała Xid 32 — poprawiona (2026-11-21)

Test na serwerze bez gry, `systemctl --user restart gpu-renderd` co 7 s i `journalctl -k`: **12 restartów z sondą (0.3.37-0.3.52): 1 Xid 32 przypisany `gpu-probe`** (kanał 0x0c); **12 restartów bez sondy (`GPU_RENDERD_PROBE=0`): 0 Xid**. Czyli sprawdzenie karty przy starcie, dodane jako samoleczenie po DEVICE_LOST, samo uszkadzało stan sterownika w ok. 1 na 12 startów (po takim Xid kolejna gra się wywracała, serwer kończył się kodem 75).
Poprawka (`vk/health.rs`): przed `vkDestroyDevice` jest `vkDeviceWaitIdle` i 300 ms zwłoki (`GPU_RENDERD_PROBE_GENTLE=0` wraca do natychmiastowego niszczenia). Pomiar po poprawce: **0 Xid w 40 restartach**. Siła dowodu: przy wcześniejszej częstości ok. 8% szansa na 40 zer z rzędu wynosi ok. 3,5%, więc poprawka jest bardzo prawdopodobnie skuteczna, ale to nie dowód; przy 12 restartach różnica 1 vs 0 jest sama w sobie słaba. Pozostałe Xid 32 z dnia (od `gpu-renderd` w trakcie gry) nie pochodzą z sondy i pozostają niewyjaśnione.

## Grupowanie odczytów różnicowych na serwerze: zostaje włączone (2026-11-21)

`GPU_RENDERD_NO_GROUP=1` wyłącza grupowanie kolejnych żądań odczytu różnic (jedna kopia GPU i jeden płotek na grupę zamiast na pamięć). Cztery przebiegi naprzemiennie na stabilnym serwerze (poprzednia próba padła przez Xid): bez grupowania **31,69 / 31,63 FPS** (min 19,2 / 19,4), z grupowaniem (domyślnie) **32,50 / 32,65 FPS** (min 19,9 / 20,6). Zysk ok. +0,9 FPS (+2,7%), spójny w obu parach, więc domyślne zostaje; opcja wyłączająca zostaje do pomiarów.

## Xid 32 przy starcie gry: przyczyna (nakładanie niszczenia instancji z `vkCreateDevice`) i poprawka (2026-11-21)

Dziennik serwera: wszystkie 6 zdarzeń `VK_ERROR_DEVICE_LOST` dnia wystąpiło w `vkCreateDevice` gry (nie w trakcie rozgrywki), zawsze po tej samej sekwencji: seria krótkich sesji sond (Heroic, launchery) tworzących i zamykających instancje, gra tworzy 5 instancji naraz, potem fatalne `vkCreateDevice`; kernel loguje Xid 32 na kanale 0x01000009, serwer kończy się kodem 75. Przed `vkCreateDevice` NIE było zniszczenia urządzenia, więc pierwsza hipoteza (create zaraz po destroy urządzenia) była błędna.
Narzędzie: pętla samych startów gry (`startloop.sh`: ubicie gry, start Heroic i gry, 40 s, zliczanie Xid i wyjść serwera). **Seria 1** (serializacja tylko `vkCreateDevice`/`vkDestroyDevice`/sprzątania urządzeń, odstęp 250 ms): **4 Xid i 3 wyjścia serwera w 24 startach** (ok. 17%, błąd odtwarzalny; test syntetyczny z vkcube, 90 uruchomień, nie odtwarzał go wcale). **Seria 2** (do tego `vkDestroyInstance` pod tą samą blokadą, `with_lifecycle` w `backend.rs`): **0 Xid i 0 wyjść serwera w 30 startach**. Przy częstości 17% szansa na 30 zer z rzędu to ok. 0,4%, więc poprawka jest bardzo prawdopodobnie skuteczna; nie ma tu serii kontrolnej bez blokady w tej samej pętli (poprzednia seria miała tylko część blokady), więc zysk wymaga potwierdzenia na dłuższym czasie normalnej pracy. Benchmark po zmianie: **32,61 i 32,26 FPS** (bez regresji). Mechanizm (hipoteza, niedowiedziona): sterownik NVIDIA nie znosi `vkDestroyInstance` jednej sesji nakładającego się z `vkCreateDevice` drugiej w tym samym procesie.

## Wątek nadawczy klienta i robotnicy kompresji — obecny układ jest najlepszy (2026-11-21)

Ostatni niesprawdzony pomysł z klasy "zbieranie dla przepustowości kosztem opóźnienia": kolejka wybudzeń wątku nadawczego (każdy zapis pamięci przechodzi przez dwa wybudzenia: robotnik kompresji, potem nadawca). Istniejące opcje A/B (po stabilizacji serwera): baza **32,25 / 32,73 FPS**; `GPU_ICD_SYNC_SEND=1` (wysyłka wprost z wątku gry, bez wątku nadawczego) **27,02** (min 17,4; −16%); `GPU_ICD_FRAMERS=0` (bez wątków kompresji) 32,49 (neutralne); `GPU_ICD_FRAMERS=4` 30,99 (−4%). Wniosek: asynchroniczny nadawca daje dużo, więcej robotników szkodzi (konkurują o 12 wątków CPU z grą), mniej nie zmienia; domyślne (2 robotników) zostaje. Klasa pomysłów zamknięta: jedyny realny zysk dały odpowiedzi serwera wysyłane od razu (+9%) i grupowanie odczytów (+2,7%).

## Uwaga do porównania z natywnym wynikiem 86 FPS (2026-11-21)
Lokalna karta desktopa to GTX 1080 Ti (11 GB), SŁABSZA od RTX 3090 Ti (24 GB) na serwerze. Natywne 85,92 FPS (minimum 64,76, maks. 110,61) zmierzono na presecie Niska z FSR auto, bez RT; przy takich ustawieniach i procesorze Ryzen 5 1600X gra jest prawdopodobnie ograniczona CPU (minimum 65 FPS), więc mocniejsza karta lokalnie dałaby w tym teście pewnie niewiele więcej (niezmierzone: nie da się wstawić 3090 Ti do desktopa). Zdalnie GPU serwera jest zajęte tylko 10-15%, więc wąskim gardłem nie jest moc karty, tylko czas obiegów po sieci i szeregowe przetwarzanie po obu stronach. Ustawienia obecnych testów zdalnych (`UserSettings.json`: preset Custom, cienie i LOD Medium, tłum Low, FSR auto, bez RT, pełny ekran 1080p) mogą różnić się od natywnego przebiegu na presecie Niska; nie porównano ich punkt po punkcie, więc 32 vs 86 FPS jest przybliżeniem co do ustawień grafiki (niezweryfikowane, czy są identyczne).

## Opcje, bez których gra traci 30%: SEM_CACHE i LAZY_SCAN_ONE domyślnie włączone; limit połączeń 64 (2026-11-21)

**Pomiar konfiguracji (Cyberpunk, ten sam dzień):** baza 32,92 / 31,98 FPS; bez diagnostyk (`GPU_ICD_STATS`, `GPU_ICD_LOG`) 32,58 (debug nic nie kosztuje); **bez `GPU_ICD_SEM_CACHE` 22,98 FPS (-30%)**; **bez `GPU_ICD_LAZY_SCAN_ONE` 23,00 FPS (-30%)**. Obie były opcjami WYŁĄCZONYMI domyślnie i włączał je tylko plik konfiguracji Cyberpunka w Heroic; konfiguracja Rise of the Tomb Raider ich nie miała (zamiast tego miała neutralny `GPU_ICD_DESC_PREFETCH=1`), więc wyniki gier nie były mierzone tą samą konfiguracją, a każda nowa gra traciłaby jedną trzecią. Zmiana: obie opcje domyślnie włączone (`=0` wyłącza; `on_by_default` w `fwd.rs`). Pomiar po zmianie, bez tych zmiennych w konfiguracji gry: **32,09 / 32,46 / 31,93 FPS**.
**Limit połączeń serwera:** w logu 96 wpisów `odrzucone (limit 16 połączeń)`, w tym serie przy startach gry (krótkie sesje sond i procesów gry; każda sesja używa teraz kilku połączeń: sterujące, wideo, odczyty); odrzucone połączenie kończyło się `vkCreateInstance` -> "Failed to create Vulkan instance" w DXVK/launcherze i wywrotką gry (dwie nieudane próby z rzędu). Zmiana: `--max-conns 64` w jednostce usługi i w domyślnej wartości kodu. Po zmianie 0 odrzuceń.
**Pozostały Xid 32:** po szeregowaniu cyklu życia urządzeń (0.3.55) jeden Xid (kanał 0x17, `vkCreateDevice` -> -3, bez wyjścia 75) w ok. 50 startach gry po wdrożeniu (ok. 2%; wcześniej ok. 17% w pętli startów i ok. 8% w ciągu dnia). Poprawa, nie usunięcie.

## Rise of the Tomb Raider: zawieszenia przez kanał odczytów w grze z wieloma instancjami Vulkana (2026-11-21)

Pierwszy przebieg po włączeniu domyślnych opcji (SEM_CACHE, LAZY_SCAN_ONE) i limicie połączeń 64: gra ruszyła i stanęła. Log ICD (`icd-rottr.log`): `serwer odmówił (-1, read failed)` dla pamięci 0x100f0d, potem dwa razy `timeout waiting for the control stream` po 30 s; `pobrania leniwe: 5 razy, łączny czas 60011 ms` (2 x 30 s), serwer w tym czasie bezczynny (0% CPU, 0 żądań). Przyczyna: kanał odczytów był JEDEN globalny, podłączony do sesji swapchaina, a odczyt leniwy używał go dla pamięci z dowolnej instancji; Tomb Raider (DXVK) tworzy kilka instancji, więc pamięć innej sesji trafiała do niewłaściwego widoku (`read failed`) albo czekała na numer ramki z niewłaściwego strumienia sterującego do limitu 30 s. W Cyberpunku (jedna sesja z grafiką) tego nie widać.
Wynik tego przebiegu (biblioteka sprzed poprawki, dwie zawiechy po 30 s), wbudowany benchmark Full HD: Spine of the Mountain **14,6 FPS** (min 0,0, maks 119,9, 1745 klatek) — zniekształcone zawiechami; Prophet's Tomb **32,0 FPS** (min 6,1, 790 klatek); Geothermal Valley **24,0 FPS** (min 4,3, 716 klatek). Poprzednio (0.3.28): Mountain Peak 29,9, Syria 28,6, Geothermal 26,6, razem 28,3. Przebieg do powtórzenia po poprawce.
Poprawka: `ReadChan` zna sesję (`InstState::session`), `readchan::get_for(session)` zwraca kanał tylko dla pamięci własnej sesji (inne idą strumieniem sterującym), a limit czekania serwera na zależność spadł z 30 do 3 s (klient 6 s). Testy zielone; NIE zweryfikowane jeszcze na prawdziwym przebiegu Tomb Raidera.

## Rise of the Tomb Raider po poprawkach kanału odczytów (2026-11-21, trzy przebiegi, pełny ekran Full HD)

Wbudowany benchmark, trzy sceny; "Overall" gry to średnia z trzech scen (sprawdzone na wyniku 0.3.28: 29,90, 28,62, 26,55 -> 28,34). Nazwy plików wyników w tej wersji gry: SpineOfTheMountain, ProphetsTomb, GeothermalValley (prawdopodobnie te same przebiegi co Mountain Peak, Syria, Geothermal Valley z poprzedniego pomiaru; nie potwierdzone osobno).

| Przebieg | Spine | Prophet's Tomb | Geothermal | Overall (średnia scen) | Uwaga |
|---|---|---|---|---|---|
| 0.3.28 (rano, bez dzisiejszych poprawek) | 29,90 | 28,62 | 26,55 | **28,34** | punkt odniesienia |
| 1 (domyślne opcje, kanał globalny) | 14,6 (min 0,0) | 32,0 | 24,0 | 23,5 | dwie zawiechy po 30 s |
| 2 (kanał per sesja, limit 3 s) | 45,6 (min 0,3) | 31,6 | 24,2 | 33,8 | 4 odczyty po 3 s |
| 3 (+ poprawka zawijania numerów ramek) | **56,3** (min 9,5, maks 122,4) | 28,3 (min 4,7) | 25,0 (min 5,6) | **36,5** | 0 ostrzeżeń watchdoga, 1 powrót do strumienia sterującego |

Wniosek: Overall 28,3 -> **36,5 FPS (+29%)**; dominuje pierwsza scena (+88%), scena 2 bez zmiany (28,3 vs 28,6, w szumie), scena 3 nieco gorsza (25,0 vs 26,6; ok. -6%, nie zbadane: ta scena ma minima 4-6 FPS, czyli krótkie przestoje). Statystyki ICD przebiegu 3: 3738 pobrań leniwych po średnio 6,9 ms (233 MB), kanał odczytów 2534 odczytów po średnio 5,0 ms, 1 ucieczka do strumienia sterującego (w przebiegu 2: 4 timeouty po 3 s, w 1: dwa po 30 s). Mechanizm poprawki: `read_dep` liczy wiek zależności licznikiem bez zawijania (zależność starsza niż 40 tys. ramek jest zastępowana bezpieczną wartością), a serwer (`readview::reached`) uznaje za wykonane ramki do 45 tys. wstecz (okno asymetryczne zamiast ±32768). Przebiegi 2 i 3 były wykonane ręcznie przez użytkownika; różnice między przebiegami (np. scena 2: 31,6 vs 28,3) mogą mieścić się w szumie (nie powtarzano wielokrotnie).

## Rise of the Tomb Raider: kodek zapisów (zstd vs LZ4), vsync wyłączony i hamulec klatek w locie (2026-11-21)

Zrzuty (użytkownik): ustawienia gry [`rottr-display-settings.jpg`](rottr-display-settings.jpg) (DX11, pełny ekran, 1920x1080, VSync OFF, FXAA, monitor "NVIDIA GeForce RTX 3090 Ti"), wynik [`rottr-vsync-off-result.png`](rottr-vsync-off-result.png) (przebieg 5: Mountain Peak 29,51, Syria 32,18, Geothermal Valley 23,81, Overall 28,60). Nazwy scen potwierdzone: pliki wyników `SpineOfTheMountain` = Mountain Peak, `ProphetsTomb` = Syria.

| Przebieg | Kodek | vsync | Mountain Peak | Syria | Geothermal | Overall | Ruch na łączu | Uwaga |
|---|---|---|---|---|---|---|---|---|
| 3 | LZ4 (domyślnie na łączu >= 2 Gb/s) | ON | 56,3 | 28,3 | 25,0 | 36,5 | 200-250 MB/s | łącze zajęte w 70-80% |
| 4 | zstd z prefiksem (wymuszone) | ON | 56,9 | 33,9 | 30,8 | **40,5** | 100-125 MB/s | scena 3 +23% |
| 5 | adaptacyjny (zostaje zstd) | OFF | 29,5 | 32,2 | 23,8 | 28,6 | ok. 210 MB/s | hamulec klatek wyłączony od startu: opóźnienie 0,7-7 s, menu "lepkie", `hamowanie gry: 0 razy` |
| 6 | adaptacyjny (zostaje zstd) | OFF | 56,7 | 33,0 | 30,9 | **40,2** | 100-130 MB/s | po poprawce hamulca: opóźnienie ok. 50 ms, hamowanie 190-300 razy na okno, menu żywe |

Wnioski: (1) **Kodek:** Tomb Raider (upload 440-500 MB/s surowo) potrzebuje zstd z prefiksem; Cyberpunk (ok. 25-55 MB/s) zyskuje ok. 3% na LZ4. Mechanizm adaptacyjny w `link.rs`: start od zstd, jednokierunkowe przejście na LZ4 po 6 kolejnych oknach 5 s z ruchem 5-60 MB/s na łączu >= 2 Gb/s (testy jednostkowe progu i zerowania); Cyberpunk przeszedł na LZ4 (32,2 FPS), Tomb Raider zostaje przy zstd (log bez przejścia). (2) **Hamulec klatek w locie** (`latency::wait_for_room`, max 3 niewyświetlone): wyłączał się na stałe po 5 kolejnych przekroczeniach czasu, co zdarza się przy starcie gry, zanim ruszy wideo (przebieg 5: od pierwszego okna opóźnienie 700-960 ms, `hamowanie 0 razy`; przebieg 4 z tym samym kodem: 50 ms, hamowanie 170-300 razy). Poprawka: wyświetlenie klatki włącza hamowanie z powrotem (`s.throttle_off = false` w `latency::shown`, test jednostkowy). (3) **vsync:** z wyłączonym vsync i działającym hamulcem wynik nie zmienia się (40,2 vs 40,5), bo scena 1 i tak nie przekracza 60 FPS; wyłączony vsync daje przy tym prawdziwą moc karty w porównaniu natywnym (limit 60 Hz uciął wcześniejszy natywny wynik 59,99).

## Rise of the Tomb Raider: natywna GTX 1080 Ti vs zdalna RTX 3090 Ti, vsync wyłączony, czasy klatek (2026-11-21, 0.3.62)

Ten sam komputer (Ryzen 5 1600X), DX11, 1920x1080 pełny ekran, VSync OFF, wbudowany benchmark; natywnie = konfiguracja Heroic bez zmiennych sterownika (`UMU_NO_RUNTIME` tylko), jeden przebieg (pierwszy, cache shaderów lokalnej karty mógł być zimny); zdalnie = przebieg 6 (zstd, hamulec klatek działa).

| Scena | Natywnie 1080 Ti (min / maks) | Zdalnie 3090 Ti (min / maks) | zdalnie / natywnie |
|---|---|---|---|
| Mountain Peak | **165,2** FPS (46,9 / 242,2), 4841 klatek | 56,7 (14,3 / 109,5), 1678 | 34% |
| Syria | **82,0** (35,7 / 115,7), 2009 klatek | 33,0 (6,4 / 51,7), 816 | 40% |
| Geothermal Valley | **76,9** (41,4 / 122,3), 2266 klatek | 30,9 (6,4 / 62,9), 917 | 40% |
| **Overall (średnia scen)** | **108,0** | **40,2** | **37%** |

Benchmark jest ograniczony czasem (ok. 30 s na scenę, 4841 vs 1678 klatek w tym samym czasie), więc stosunek klatek = stosunek FPS. Lokalna karta jest SŁABSZA od serwerowej (1080 Ti vs 3090 Ti), ale scena 1 daje natywnie 165 FPS, czyli przy 1080p jest raczej ograniczona procesorem; zdalnie GPU serwera jest mało obciążone, więc różnica nie wynika z mocy karty.

**Równomierność klatek (pliki `*_frametimes_*.txt`, odstępy w ms):**

| Scena | Źródło | p50 | p95 | p99 | maks | średnia najgorszego 1% |
|---|---|---|---|---|---|---|
| Mountain Peak | zdalnie | 17,2 | 26,0 | 33,2 | 70,1 | 41,8 |
| Mountain Peak | natywnie | 5,8 | 7,6 | 10,5 | 25,1 | 12,9 |
| Syria | zdalnie | 28,5 | 45,7 | 67,3 | 157,3 | 100,1 |
| Syria | natywnie | 11,6 | 17,7 | 21,9 | 28,0 | 24,6 |
| Geothermal | zdalnie | 27,0 | **74,7** | **107,0** | 156,9 | 123,5 |
| Geothermal | natywnie | 12,6 | 17,6 | 19,6 | 24,2 | 20,6 |

Nierówny ruch kamery jest więc realny i największy w scenie 3: zdalnie p95 to 2,8x mediany (natywnie 1,4x), 68 klatek > 60 ms z 917 (7%, razem 5,8 s z 29,7 s), skoki nieregularne (odstępy 1-26 klatek, brak okresowości). Kształt przebiegu w czasie jest taki sam jak natywnie, tylko ok. 2,4x wolniejszy (średnia klatka co 2 s: natywnie 16,7 -> 10,4 ms, zdalnie 41,1 -> 24,6 ms), czyli narzut zdalny rośnie proporcjonalnie do pracy gry na klatkę (liczby poleceń i punktów synchronizacji), a nie jest stałym kosztem na klatkę. Wniosek na plan: przy ok. 2,4x natywnego czasu klatki dalsze zyski daje redukcja kosztu per polecenie/synchronizację po stronie klienta, a rozrzut klatek w scenie 3 (p99 107 ms) wymaga osobnego zbadania skoków (kandydaci: przejścia pamięci leniwej, zapisy dużych porcji w jednej klatce).

## Skoki klatek w Rise of the Tomb Raider: zrzuty zdarzeń (GPU_ICD_SPIKES), hamulec poza blokadą, kodek bez przełączania, kolejka wysyłki (2026-11-21, 0.3.64)

Nowe narzędzie `GPU_ICD_SPIKES=<ms>` (`timeline.rs`): stały bufor 6000 ostatnich zdarzeń; przy odstępie między `vkQueuePresentKHR` ponad progiem do logu trafia lista najdłuższych zdarzeń z tego odstępu (`[spike]`, wątek, polecenie, czas, czekanie na blokadę), do 14 zrzutów na sesję; zdarzenia: wywołania ICD, `lazy_fetch`, `hamulec_klatek`.

| Przebieg (vsync OFF, 1080p, DX11) | Zmiana | Mountain Peak | Syria | Geothermal | Overall | Geothermal p95 / p99 (ms), skoki > 60 ms | Ruch na łączu |
|---|---|---|---|---|---|---|---|
| 6 | zstd, hamulec naprawiony | 56,7 | 33,0 | 30,9 | 40,2 | 74,7 / 107, 68 z 917 | 100-130 MB/s |
| 7 | **przejście na LZ4 po 30 s** | 54,6 | 28,4 | 24,8 | 35,9 | n/d | **ok. 230 MB/s** (łącze prawie nasycone) |
| 8 | zstd bez przełączania + hamulec czeka PRZED blokadą | 59,4 | 35,8 | 28,3 | **41,2** | 60,8 / 78,7, 45 z 839 | 85-133 MB/s |
| 9 | + kolejka wysyłki 8 MB (`GPU_ICD_SENDQ_MB=8`) | 59,7 | 34,9 | 27,9 | 40,8 | 56,8 / 77,3, 28 z 830 | 37-118 MB/s |
| natywnie 1080 Ti (dla porównania) | - | 165,2 | 82,0 | 76,9 | 108,0 | p95 17,6 / p99 19,6 | - |

Ustalenia ze zrzutów: (1) **Hamulec klatek w locie (do 100 ms) czekał WEWNĄTRZ blokady instancji** (`hamulec_klatek` 100 ms oraz `vkResetCommandPool`/`vkEndCommandBuffer` z `lock` = 100 ms w tym samym odstępie): przeniesiony przed blokadę (`queue_present`), po zmianie `lock 0` we wszystkich zrzutach. (2) **Jednokierunkowe przejście na LZ4 zawiodło**: gra miała w menu i ładowaniu 30 s lekkiego ruchu (5-60 MB/s), potem ciężki; utknęła w LZ4 (przebieg 7, 230 MB/s, 35,9 FPS); automatyczne przełączanie wyłączone domyślnie (`GPU_ICD_MEM_ADAPT=1` włącza), domyślnie zstd; koszt dla Cyberpunka ok. 3%. (3) **Zapytania z odpowiedzią** (`vkGetDescriptorEXT`, `vkGetImageMemoryRequirements2`, `vkGetSemaphoreCounterValue`) potrafią trwać 30-95 ms w fazie ładowania: stoją za dużymi zapisami w kolejce wysyłki (jedno połączenie, FIFO, kolejka do 64 MB = 0,3-0,6 s przy 100-230 MB/s) i na blokadzie instancji. Kolejka 8 MB nie dała zysku (40,8 vs 41,2, w szumie), a zrzut z przebiegu 9 pokazuje `vkCreateImage` z `lock` 69 ms: podejrzenie, że producent zapisów czeka na miejsce w kolejce (`enqueue_item`) TRZYMAJĄC blokadę instancji, więc mniejsza kolejka nasila blokowanie pod blokadą zamiast je zmniejszać. Domyślne 64 MB zostaje; kandydat na następny krok: backpressure przed blokadą instancji albo osobna ścieżka dla zapytań. (4) Pierwsze 3 zrzuty każdej sesji (`hamulec_klatek` 100 ms) to start gry, zanim ruszy wideo.

## Profil `perf` Rise of the Tomb Raider (klient i serwer) i konwersja NV12 w SSE4.1 (2026-11-21, 0.3.65)

Dwa nagrania po 8 s (`perf record --call-graph dwarf`) w scenach benchmarku (vsync OFF, zstd), serwer i desktop jednocześnie.
**Serwer** (wątek obsługi `gpu-renderd`, ok. 77-83% próbek): dekompresja zstd z prefiksem 11-12% (zapisy pamięci zależne od lustra, nie dają się przenieść do wątku odczytu), **CRC32C 5,7-8,4%** (weryfikacja sumy rozpakowanych zapisów, zabezpieczenie przed rozjazdem luster; zostaje), `page_hash` 3-4%, **zegar systemowy ok. 8%** (`vdso` + `clock_gettime`: liczniki czasu `GPU_RENDERD_STATS=1` mierzą każde żądanie, a Tomb Raider ma ich ok. 6,6 mln zapisów pamięci na przebieg wobec 0,6 mln w Cyberpunku; w Cyberpunku poniżej 0,5%, dlatego wcześniej nie wyszło). Statystyki serwera w usłudze wyłączone (zostają do diagnostyki: dopisać `Environment=GPU_RENDERD_STATS=1`).
**Klient** (proces gry, 111 wątków): gra (wątki `cdcMulticore`, `ROTTR.exe`) ok. 45% próbek, nasz sterownik ok. 36%: wątek wideo ok. 15%, z czego **`gpu_vaapi::nv12::convert_band` 11,7%** (konwersja NV12 -> BGRA każdej odebranej klatki na CPU, kod skalarny), dwa wątki kompresji zstd (`ZSTD_compressBlock_fast_extDict`) ok. 14-17%, wątki DXVK w naszej bibliotece ok. 6%.
**Poprawka:** rdzeń konwersji przepisany na SSE4.1 (`crates/gpu-vaapi/src/nv12.rs`, `simd::pairs_sse41`: osiem pikseli na iterację, saturujące pakowanie równoważne `clamp(0,255)`, brzegi i procesory bez SSE4.1 liczy wersja skalarna; test zgodności bajt w bajt z wersją wzorcową na losowych danych, nieparzyste przycięcia, 1920 px). Pomiar (`bench_1080p_simd_vs_scalar`, ignorowany test): na jednym rdzeniu 7,79 -> 2,63 ms na klatkę 1080p (**3,0x**), na 8 wątkach 3,45 -> 1,74 ms (2,0x; dominuje narzut wątków i alokacji). Cyberpunk po zmianie (razem ze statystykami serwera wyłączonymi): **32,80 / 32,08 FPS** wobec 31,28 / 31,71 przed (ok. +1 FPS). Przebieg Tomb Raidera z samym SIMD i wyłączonymi statystykami: do zmierzenia (przebieg z `perf` miał 40,4, ale nagrywanie samo spowalnia).

### Przebieg Tomb Raidera po SIMD konwersji NV12 i bez statystyk serwera (2026-11-21)
Overall **42,13 FPS** (Mountain Peak 59,9, Syria 36,5, Geothermal Valley 30,0; poprzednio 41,17 / 59,4 / 35,8 / 28,3, czyli +2,3%); scena 3: p95 56,4 ms, p99 69,9 ms (było 60,8 / 78,7), 32 klatki > 60 ms z 892 (było 45 z 839). Zero Xid, bez przejścia na LZ4, ruch na łączu 87-126 MB/s. Czas CPU wątków procesu gry w sesji (s): `gpu-icd-frame0` 64,5 i `gpu-icd-frame1` 64,7 (kompresja zstd, razem 129 s, czyli ok. 1,3 rdzenia), `gpu-icd-video` x6 21,4 (po SIMD), `dxvk-cs` x2 56,4, `gpu-icd-send` x2 9,4; wątki gry: `ROTTR.exe` x19 117,0, `cdcMulticore` ok. 147 łącznie. Kompresja zapisów jest największym kosztem CPU naszego sterownika po stronie klienta; łącze ma zapas (87-126 z ok. 300 MB/s), więc następny test to niższy poziom zstd (`GPU_ICD_ZSTD_LEVEL=-5`).

## Adaptacyjny poziom zstd (2026-11-22, 0.3.66)

Poziom zstd dla zapisów pamięci jest dobierany do zajętości łącza (`link.rs`, `LevelAdapt`, `adapt_level`): domyślnie -5 ("fast"), poziom 1, gdy ruch w oknie 2 s przekracza 75% pojemności łącza (pojemność z prędkości interfejsu minus 6% narzutu; 1 Gb/s, gdy nieznana), powrót do -5 poniżej 50% (histereza). W odróżnieniu od przełączania kodeka (LZ4 <-> zstd z prefiksem) zmiana poziomu jest bezpieczna w obie strony: zstd z prefiksem nie niesie innego stanu niż lustro, wspólne dla wszystkich poziomów. `GPU_ICD_ZSTD_LEVEL` wymusza poziom. Test jednostkowy progów i histerezy.

| Przebieg Tomb Raidera (vsync OFF) | Mountain Peak | Syria | Geothermal | Overall | Geothermal p95 / p99, skoki > 60 ms | Ruch na łączu |
|---|---|---|---|---|---|---|
| poziom 1 (stały) | 59,9 | 36,5 | 30,0 | 42,13 | 56,4 / 69,9, 32 z 892 | 87-126 MB/s |
| poziom -5 (ręcznie) | 60,0 | 38,1 | 32,4 | 43,50 | 48,8 / 68,0, 15 z 961 | 112-201 MB/s |
| **adaptacyjny (domyślnie)** | 60,2 | 39,8 | 32,8 | **44,27** | **46,6 / 63,6, 12 z 975** | 92-197 MB/s |

Cyberpunk z adaptacyjnym poziomem (bez zmiennych w konfiguracji): 32,24 / 32,66 FPS (przed zmianą 31,3-32,8), bez regresji. Czas CPU wątków kompresji spada razem z poziomem, więc procesor klienta zostaje dla gry. Tomb Raider ogółem od rana: Overall 28,3 -> 44,3 FPS (+56%), natywnie 1080 Ti 108,0 (41%).

## Koszt kompresji zapisów po stronie klienta: co sprawdzono i dlaczego nie ma tu taniego zysku (2026-11-22)

Czas CPU wątków kompresji (`gpu-icd-frame0/1`, Tomb Raider, cała sesja): poziom 1 po 64,5 s i 64,7 s, adaptacyjny poziom -5 po 56,5 s i 56,3 s (-13%, a nie -50%, jakiego oczekiwano po trybie "fast"). Hipoteza "nowy `CCtx::create()` na każdą ramkę dominuje" (ok. 220 tys. ramek po średnio 2 KB na przebieg) została ZMIERZONA i odrzucona: mikrobenchmark 20 tys. ramek, 2 KB i 8 KB z prefiksem, poziom -5: nowy kontekst 6,6 / 21,2 µs, ponowne użycie jednego kontekstu 5,9 / 20,4 µs (1,1x). Koszt jest więc proporcjonalny do bajtów (ok. 3 µs na KB z prefiksem na wątek, ok. 300 MB/s na rdzeń przy tym wzorcu danych), a przy 440-500 MB/s surowych zapisów daje to ok. 1-1,5 rdzenia. Jedyna droga do realnej redukcji to wysyłać mniej bajtów do kompresji; kodowanie samych zmienionych bajtów (62% wysyłanych bajtów jest identyczne z poprzednim zapisem w to samo miejsce) było już analizowane (2026-11-09, `MemWriteRuns`): poprawne tylko dla pamięci, do której GPU nie może pisać, a analiza flag użycia nie kwalifikuje żadnej dużej pamięci (vkd3d daje bufferom flagi z zapisem GPU), więc zysk = 0 bez zmiany modelu spójności.
Kandydat strukturalny (nie podjęty): osobne połączenie dla dużych zapisów pamięci z barierą po numerze ramki przed `vkQueueSubmit` (jak już działa kanał odczytów `MemReadAfter`), żeby zapytania z odpowiedzią (30-95 ms w fazie ładowania) nie stały za wielomegabajtowymi zapisami w jednej kolejce TCP. Zmiana protokołu i modelu kolejności, dni pracy i ryzyko poprawności, więc tylko za wyraźną decyzją.
