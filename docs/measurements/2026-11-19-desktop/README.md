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
