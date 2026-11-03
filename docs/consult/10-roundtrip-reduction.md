# Burza mózgów: jak zmniejszyć koszt obiegów po sieci w zdalnym sterowniku Vulkan

## Kontekst (krótko)

`gpu-icd` to nasz sterownik Vulkan na kliencie (Rust, cdylib): przekazuje polecenia gry przez TCP do demona `gpu-renderd` na serwerze z
RTX 3090 Ti, który wykonuje je na prawdziwym sterowniku NVIDIA i odsyła obraz (H.264). Polecenia bez wyniku idą w partiach
(asynchronicznie), a **zapytania z wynikiem są synchroniczne: jeden obieg po sieci** (klient wysyła, czeka na odpowiedź). Jeden strumień TCP,
odpowiedzi w kolejności wysłania. Jedna blokada na całą instancję sterownika (`InstState`), trzymana przez cały czas obiegu.

Gra: Cyberpunk 2077 (DX12 -> vkd3d-proton -> `VK_EXT_descriptor_buffer`) na laptopie Ryzen 5 4500U, serwer w sieci 1 Gb/s przez kabel.
Obraz jest poprawny, ale **4-5 FPS**, a GPU, CPU (ok. 10%) i łącze (ok. 25%) NIE są wysycone: system czeka na odpowiedzi.

## Zmierzone (jeden przebieg, 1871 klatek, czyste opcje bez logowania)

- **187 obiegów po sieci na klatkę** (349 397 / 1871). Czas czekania na serwer: 244 s (130 ms na klatkę).
- Opóźnienie łącza: ping przy ciągłym ruchu średnio 0,37 ms (min 0,09 ms; adapter USB 3.0 RTL8153; wbudowana karta miała ok. 0,1 ms).
- `vkGetDescriptorEXT` (deskryptory, zapytanie z wynikiem o rozmiarze 16-64 B zależnym wyłącznie od wejścia): 400 036 trafień cache, **245 579 chybień** = obiegi.
  Rozkład chybień wg `VkDescriptorType`: **UNIFORM_BUFFER 220 195 (90%)**, SAMPLED_IMAGE 9 810, UNIFORM_TEXEL 7 525, STORAGE_BUFFER 7 527, reszta < 400.
  Wejście to (adres bufora, zakres, format) dla buforów i (uchwyt widoku) dla obrazów; klucz cache = zakodowane żądanie.
  Pozostałe zapytania: ok. 104 tys. (m.in. `vkWaitSemaphores`, `vkGetBufferDeviceAddress`, `vkGetDeviceBufferMemoryRequirements`, sync alokacje).
- Odświeżanie odwzorowanej pamięci po oczekiwaniu na GPU: 23 681 odświeżeń, 71 043 żądania zakresowe, **82 s** czasu serwera (1,16 ms każde; dane
  odczytywane strumieniowo z pamięci write-combined; zakresy średnio 9,5 MB) z 244 s czekania. Część pamięci (nie-HOST_CACHED) już pomijamy.
- Zapisy pamięci: 2,3 GB surowo, 0,63 GB na łączu (LZ4); to nie jest wąskie gardło.
- Serwer: wykonanie poleceń 8 s, zapytania 2,7 s, kodowanie klatek 2,4 s: serwer jest prawie bezczynny.

## Już zrobione (nie proponuj ponownie)

cache wymagań pamięci (klucz = parametry obiektu), cache deskryptorów (klucz = zakodowane żądanie), spekulatywne odpowiedzi
`vkAllocateDescriptorSets` (księgowość pul po stronie klienta), kompresja LZ4 zapisów pamięci, odświeżanie tylko zakresów zapisywalnych przez GPU,
limit klatek w locie (3), potok klatek na drugim połączeniu, żądania odświeżenia pamięci wysyłane w tym samym obiegu co `Sync`.

## Ograniczenia, których nie wolno naruszyć

- Poprawność: gra ma widzieć dokładnie to, co na lokalnej karcie. Żadnych "zgadnięć" bez weryfikacji.
- Bez zmiany gry; sterownik musi działać dla dowolnej aplikacji Vulkan (ATS, Unity, vkd3d, DXVK).
- Kod w Rust, bez nowych ciężkich zależności. Klient to biblioteka ładowana do procesu gry (wiele wątków aplikacji woła API równolegle).

## Pytanie

Zaproponuj **konkretne techniki** obniżenia liczby obiegów (z 187 na klatkę) i/lub kosztu jednego obiegu, uszeregowane wg (zysk / ryzyko / koszt
implementacji). Dla każdej: mechanizm, jak zachować poprawność, jak to zweryfikować, jaki zysk realistycznie (w obiegach na klatkę lub ms).
Szczególnie interesują nas:

1. 90% chybień to deskryptory buforów stałych (UNIFORM_BUFFER, wejście: adres+zakres+format). Czy da się je liczyć lokalnie (np. wyuczyć/zweryfikować funkcję
   sondowaniem serwera), przewidywać z wyprzedzeniem (prefetch) albo ich zapytania grupować?
2. Wielowątkowość: blokada trzymana przez cały obieg serializuje wątki gry. Czy odpowiedzi można dopasowywać po numerze zapytania i zwalniać blokadę na czas oczekiwania?
3. Koszt jednego obiegu: busy-poll zamiast uśpienia wątku, `SO_BUSY_POLL`, `TCP_QUICKACK`, koalescencja przerwań (`ethtool -C`), przypięcie wątków, zegar procesora.
4. Pozostałe ~104 tys. zapytań na sesję: które klasy da się spekulować lub odpowiadać lokalnie (stan semaforów czasowych, adresy buforów, wymagania pamięci)?
5. Odczyt leniwy pamięci (pobieranie danych z serwera dopiero przy odczycie przez aplikację, np. `userfaultfd` na pamięci współdzielonej `memfd`): czy warto i jak uniknąć zakleszczeń?
6. Cokolwiek, czego tu nie wymieniono, a mogłoby dać duży zysk (inne podejście do architektury, np. wykonywanie fragmentu logiki gry/sterownika po stronie serwera).

Odpowiedz po polsku, zwięźle (maks. ok. 60 linii), numerowaną listą.
