<!-- i18n-switch -->
[English](DESIGN-bulk-channel.en.md) · **Polski** · [Español](DESIGN-bulk-channel.es.md)
<!-- /i18n-switch -->

# Osobny kanał dla dużych zapisów pamięci (projekt, niewdrożony)

Status: **plan zapisany 2026-11-22, nic z tego nie jest zaimplementowane** poza instrumentacją (histogram czasu zapytań, punkt 0 niżej). Testy i decyzja o wdrożeniu po
pomiarach na drugiej grze (Shadow of the Tomb Raider) i na Cyberpunku. Cel: płynność (mniej klatek > 60 ms przy ładowaniu zasobów), nie średni FPS.

## 1. Problem

Wszystko, co klient wysyła do serwera, idzie jednym połączeniem TCP w jednej kolejce FIFO (`Out` w `crates/gpu-icd/src/link.rs`, pojemność `MAX_QUEUED_BYTES` = 64 MB,
opcja `GPU_ICD_SENDQ_MB`): partie poleceń (`Cmds`), zapisy pamięci (`MemWrite*`, kompresowane przez wątki `gpu-icd-frameN`) i zapytania z odpowiedzią (`Query`, `Sync`).
Zapytanie, które wchodzi do kolejki, czeka za wszystkim, co już w niej jest, a po stronie serwera jeden wątek wykonuje żądania po kolei.

Zmierzone (Rise of the Tomb Raider, DX11/DXVK, Ryzen 5 1600X, 2,5 Gb/s, vsync off, 2026-11-21/22):

* średnio żądanie z odpowiedzią czeka w kolejce 261 µs, a od zapisu do odpowiedzi 798 µs, czyli średni koszt kolejki jest mały (ok. 0,4 ms na klatkę);
* ogon: zrzuty `GPU_ICD_SPIKES=60` pokazują w fazie ładowania zasobów zapytania `vkGetImageMemoryRequirements2` 32-95 ms, `vkGetDescriptorEXT` 43-46 ms, `vkCreateImage`
  z czekaniem 69 ms na blokadę instancji, `vkGetSemaphoreCounterValue` 71 ms; w scenach benchmarku 11-12 klatek > 60 ms na ok. 975 (scena 3 i 2), p99 klatki 62-64 ms przy p50 23-29 ms;
* przy nasyceniu łącza (przebieg z LZ4, 230 MB/s) te czasy rosły do 100 ms i klatki stawały (`hamulec_klatek` 100 ms).

Spodziewany zysk: średni FPS +1-3% (górna granica, bo średni koszt kolejki to ok. 1% klatki), natomiast **płynność** (ogon rozkładu) może się wyraźnie poprawić.

## 2. Pomysł

Duże zapisy pamięci idą osobnym połączeniem (`AttachBulk{session}`, op do przydzielenia), a strumień sterujący niesie tylko polecenia i zapytania, więc zapytanie nie stoi za
megabajtami zapisów. Spójność zapewniają numery, tak jak w istniejącym kanale odczytów (`AttachReads`, `MemReadAfter`, `ReadView`):

1. **Bariera przed użyciem zapisów.** Klient numeruje zapisy na kanale bulk (u64, bez zawijania: błąd 16-bitowego numeru ramki, który wyszedł 2026-11-21, nie może wrócić).
   Przed partią poleceń, która zawiera `vkQueueSubmit*` (i przed każdym żądaniem odczytu pamięci), wstawia `BulkBarrier{upto}`; serwer wykonuje barierę, czekając aż wątek bulk
   zastosuje wszystkie zapisy do `upto`. To jest dokładnie dzisiejsza gwarancja kolejności ("zapisy przed zgłoszeniem") wyrażona numerem zamiast pozycją w jednym strumieniu.
2. **Zapis po alokacji.** Zapis na kanale bulk niesie `after_ctrl` (numer ramki sterującej, po której pamięć istnieje i jest odwzorowana); wątek bulk czeka na `ReadView::wait_for`
   (te same funkcje i okno asymetryczne, co kanał odczytów) zanim zapisze. Bez tego zapis mógłby dotrzeć przed `vkAllocateMemory`/`vkMapMemory`.
3. **Zwolnienie pamięci.** Przed `vkFreeMemory`/`vkUnmapMemory` bariera jak w pkt 1 (zapisy do tej pamięci muszą się skończyć).
4. **Lustro (prefiks zstd).** Zapisy w obrębie kanału bulk są uporządkowane (jedno połączenie), więc lustro klienta i serwera pozostaje zgodne; zapisy do TEJ SAMEJ pamięci nigdy nie
   idą jednocześnie dwiema drogami (kanał bulk zastępuje strumień sterujący dla zapisów pamięci w całości, nie tylko dla dużych).
5. **Wątek bulk po stronie serwera** wymaga dostępu do odwzorowań i lustra: dziś lustro żyje w `Replayer` (`&mut`, wątek obsługi). Trzeba je przenieść do współdzielonej struktury
   (`Arc`, blokada per pamięć) albo oddać wątkowi bulk na wyłączność (wtedy odczyty różnicowe, które dziś używają lustra, też muszą przez niego przechodzić). To jest najtrudniejszy
   element projektu.

## 3. Czego to NIE rozwiązuje

* Zapytań, które czekają na samą pracę serwera (wątek obsługi wykonuje jedno żądanie na raz, replay ok. 7 ms/klatkę) ani na globalną blokadę instancji po stronie klienta
  (`vkCreateImage` czekał 69 ms na blokadę; podejrzenie: producent zapisów czeka na miejsce w kolejce wysyłki trzymając ją, patrz niżej).
* Kosztu CPU kompresji (ok. 1-1,5 rdzenia przy 440-500 MB/s surowych zapisów) ani konwersji wideo.

Warto sprawdzić PRZED budową kanału: czy producent zapisów czeka na miejsce w kolejce (`enqueue_item`, `space.wait`) **trzymając blokadę instancji** (wołane z `send_mem` pod
blokadą). Jeśli tak, to część 30-95 ms to blokada, a nie kolejka TCP, i tańsze jest przeniesienie czekania przed blokadę (jak zrobiono z hamulcem klatek: `hamulec_klatek` 100 ms
trzymany pod blokadą zamrażał wszystkie wątki).

## 4. Plan i kolejność

0. **Instrumentacja (zrobione, 0.3.68):** histogram czasu zapytań z obiegiem po sieci (`czas zapytań z obiegiem po sieci: ...` w logu ICD, przedziały <1, <2, <5, <10, <20, <50, <100, >=100 ms
   oraz p50/p95/p99/maks). To jest miara "przed" i "po".
1. **Diagnoza blokady (pół dnia):** licznik czasu `enqueue_item` czekającego na miejsce i czy to dzieje się pod blokadą instancji; w zrzutach skoków (`GPU_ICD_SPIKES`) rozdzielić
   "kolejka TCP" od "blokada". Wynik decyduje, czy kanał bulk jest w ogóle potrzebny.
2. **Serwer:** wątek bulk z kanałem `AttachBulk`, bariera `BulkBarrier`, `after_ctrl`; współdzielone lustro; testy z atrapą backendu (jak testy `conn.rs`).
3. **Klient:** zapisy na kanale bulk, numeracja u64, bariery przed `QueueSubmit*` i odczytami, zwalnianie, wyłącznik `GPU_ICD_BULK=0`, awaryjny powrót do strumienia sterującego
   po odpięciu kanału (po opróżnieniu przez barierę).
4. **Weryfikacja i obciążenie** (niżej).

Szacunek: 2-4 dni, z czego połowa to współdzielenie lustra i testy wyścigów.

## 5. Testy (wymagane przed wdrożeniem)

* Jednostkowe: kolejność zapisów na kanale bulk; bariera czeka dokładnie do `upto`; `after_ctrl` czeka na alokację; zawinięcie numerów (u64, bez błędu okna 16-bitowego);
  zerwanie kanału w trakcie (powrót do strumienia sterującego bez utraty zapisu).
* Test wyścigu zaproponowany w konsultacji (DeepSeek, 2026-11-21): zapis A (0xAA) i odczyt na drugim kanale z zależnością N, natychmiast zapis B (0xBB) na głównym; odczyt ma
  zawsze zwracać 0xAA, nigdy 0xBB ani mieszanki; z losowymi opóźnieniami i wieloma powtórzeniami; test zakleszczenia przy wielu jednoczesnych odczytach i intensywnym strumieniu.
* Tryb weryfikacji treści (analogiczny do `GPU_ICD_RUNS_VERIFY`): co N-tą ramkę serwer porównuje zawartość pamięci GPU z lustrem po zastosowaniu zapisów; każda rozbieżność to błąd.
* Obciążeniowe: pętla startów gry (`startloop.sh`, 40 startów, 0 Xid i 0 wyjść serwera), przebiegi Cyberpunka i Tomb Raidera bez błędów kolejności (rozbieżności lustra, Xid 109/32).

## 6. Kryteria sukcesu i porażki (zmierzone przed i po tym samym narzędziem)

Punkt odniesienia (2026-11-22, Tomb Raider, adaptacyjny zstd): Overall 44,3 FPS; scena 3: p95 46,6 ms, p99 63,6 ms, 12 klatek > 60 ms z 975; Syria: p99 61,7 ms, 11 z 980;
histogram zapytań z obiegiem po sieci, **Cyberpunk 2077** (0.3.68, jeden przebieg, 31,75 FPS): 45 731 zapytań, p50 < 2 ms, p95 < 10 ms, p99 < 50 ms, maks 449,7 ms; przedziały
[<1, <2, <5, <10, <20, <50, <100, >=100 ms] = [15964, 7609, 15745, 4196, 1306, 877, 28, 6]. Czyli 2% zapytań (911 z 45 731) trwa >= 20 ms; przy szacunku ok. 35 ms na takie zapytanie
i ok. 3 ms na pozostałe ogon to ok. 1/4 sumarycznego czasu zapytań. Histogramy z Tomb Raidera i Shadow of the Tomb Raider: do zebrania przy najbliższych przebiegach.

Sukces: (a) liczba klatek > 60 ms spada o co najmniej 30%; (b) p99 czasu zapytań spada co najmniej 2x; (c) Overall nie spada o więcej niż 1%; (d) 0 rozbieżności treści w trybie
weryfikacji, 0 Xid w 40 startach; (e) żadne wyjście kanału bulk poza dozwolone nie zmienia kolejności względem poleceń (testy wyścigu).
Porażka (wycofanie, zostaje opcja `GPU_ICD_BULK=0`): (c) lub (d) niespełnione, albo (a) i (b) bez poprawy, czyli dowód, że ogon wynikał z blokady lub pracy serwera, a nie z kolejki TCP.

## 7. Alternatywy tańsze (do sprawdzenia przed budową kanału)

* Przenieść czekanie producenta (`enqueue_item`) przed blokadę instancji (backpressure przed blokadą): mała zmiana, jeśli diagnoza z punktu 1 potwierdzi mechanizm.
* Mniejsza kolejka (`GPU_ICD_SENDQ_MB=8`) przetestowana 2026-11-21: bez zysku (40,8 vs 41,2 FPS), a skoków mniej (28 vs 45 klatek > 60 ms) — wynik niejednoznaczny przy jednym przebiegu;
  warto powtórzyć z histogramem zapytań, bo teraz jest czym je ocenić.
