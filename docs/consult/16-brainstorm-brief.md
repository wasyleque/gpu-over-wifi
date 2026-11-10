# Brief do burzy mózgów: jak skrócić czas klatki zdalnego Vulkana

## Cel i zasady
Gra działa lokalnie na słabym laptopie (Ryzen 5 4500U, 16 GB), a renderuje na zdalnej RTX 3090 Ti przez własny sterownik Vulkan (ICD) i demon na serwerze.
Zasady: tylko zdalne wykonanie poleceń Vulkan (bez streamingu wideo jako zamiennika), bez utraty jakości, bez zgadywania wyników (każda lokalna odpowiedź ma być
zweryfikowana), zmiany muszą dać się zmierzyć. Łącze: kabel 1 GbE (ok. 112 MB/s użytecznie), RTT ok. 0,2 ms.

## Architektura (skrót)
- Klient (`gpu-icd`, cdylib): ICD ogłasza 326 rozszerzeń, kodek poleceń generowany z vk.xml. Każde `vkCmd*` trafia do bufora poleceń, przy `vkEndCommandBuffer` do partii;
  polecenia klasy Query/Sync (np. `vkGetDescriptorEXT`, `vkWaitSemaphores`) czekają na odpowiedź serwera po jednym połączeniu TCP, w kolejności.
- Serwer (`gpu-renderd`): wykonuje partie sekwencyjnie (jeden wątek na połączenie), odpowiada po kolei.
- Pamięć odwzorowana (HOST_VISIBLE): klient trzyma kopię (shadow), śledzi zapisy gry (userfaultfd WP), przy `vkQueueSubmit` wysyła zmienione strony (zstd poziom 1,
  od teraz z prefiksem = poprzednia wysłana treść tego samego zakresu; −42% bajtów w pomiarze offline), po oczekiwaniu na GPU odświeża tylko pamięci, które GPU mogło zapisać.
- Deskryptory: `vkGetDescriptorEXT` liczony lokalnie dla buforów i tekseli po nauczeniu i audycie wzoru (odpowiedzi serwera potwierdzają wzór 32 razy, potem audyt).
- Wątek nadawczy klienta (kolejka, kompresja równoległa w 2 wątkach), blokada instancji zwalniana na czas czekania na odpowiedź, asynchroniczne `vkAllocateDescriptorSets`
  z kontrolą puli, cache wymagań pamięci, odczyt pamięci GPU na serwerze przez kopię GPU.

## Gra testowa i wyniki (Rise of the Tomb Raider, DX12 przez vkd3d-proton, benchmark wbudowany)
Overall FPS w kolejnych przebiegach: 9,59 → 10,67 (zstd) → 11,55 → 11,94 (lokalne deskryptory formatu R32_UINT) → 12,92 (pominięcie odświeżania pamięci
deskryptorów) → 12,6–13,9 (rozrzut między przebiegami co najmniej ±4%). Sceny: Mountain Peak ok. 20-22 FPS, Syria i Geothermal Valley ok. 10 FPS.
Czas klatki ok. 85 ms; klient czeka na serwer ok. 45 ms na klatkę: `vkWaitSemaphores` ok. 7 wywołań na klatkę po ok. 6 ms (z tego ok. 1,4 ms kolejka za
zapisami pamięci przed wysłaniem, ok. 2,9 ms od zapisu do odpowiedzi: serwer wykonuje wszystko, co stoi przed zapytaniem, plus RTT).
Serwer jest zajęty tylko ok. 30% czasu (ok. 1000 poleceń rysowania na klatkę ok. 7 ms łącznie z dekodowaniem, odczyt pamięci GPU ok. 6 ms, zapisy pamięci
ok. 7 ms, kodowanie klatki do prezentacji ok. 2 ms), ale etapy działają szeregowo.
Łącze: średnio 3,3 MB na klatkę po kompresji, ale w ciężkich scenach 70-80 MB/s (ok. 7,5 MB na klatkę), czyli blisko nasycenia.
Znaczące liczby: 2,1 mln zapisów pamięci i 20 tys. `vkWaitSemaphores` na przebieg 2,8 tys. klatek; po lokalnym liczeniu deskryptorów zostają 3,5 tys. zapytań o deskryptory obrazów.

## Co już sprawdzono i odrzucono
- Cache po zawartości (okna 4 KiB wysłane wcześniej gdziekolwiek): tylko 9-11% ruchu.
- Pomijanie wysyłania linii identycznych z poprzednio wysłanymi (`MemWriteRuns`): niepoprawne, gdy GPU może zapisać pamięć (serwer różni się od kopii klienta).
- Wypychanie zapisów pamięci przy `vkEndCommandBuffer` zamiast przy `vkQueueSubmit`: brak powtarzalnego zysku.
- Odroczenie `vkFlushMappedMemoryRanges` na serwerze: brak zysku.
- Różnica XOR z poprzednią treścią: tylko −14%; prefiks zstd daje −42%.

## Techniki z dokumentacji WiCi (działający produkt tego typu; liczby od producenta)
Model kosztu: T = T_lokalne + N·RTT + D/przepustowość (N = liczba obiegów, D = bajty). Techniki: (1) grupowanie funkcji w partie wysyłane bez czekania na odpowiedź;
(2) cache po zawartości (hash-first, serwer trzyma dane z poprzednich sesji, klient wysyła tylko hash); (3) trace replay: powtarzalny ciąg wywołań (np. ta sama klatka co poprzednia)
serwer wykonuje według śladu, a klient zwraca sukces lokalnie i weryfikuje później; (4) referencje zamiast kopii, rezydencja zasobów na serwerze; (5) predykcja odpowiedzi
zapytań (odpowiedź wysyłana z wyprzedzeniem), kodowanie delta, kompresja; (6) wynik renderowania jako skompresowane wideo (u nas ten element jest inny: surowa/kodowana
klatka wraca tylko do prezentacji lokalnie, nie jest zamiennikiem zdalnego Vulkana).

## Pytania
1. Gdzie jest największy zysk w zmniejszeniu czasu klatki przy powyższych liczbach (szeregowe etapy klient→serwer→GPU→serwer→klient, 7 oczekiwań na semafor na klatkę)?
2. Jak potokować pracę (serwer wykonuje klatkę N, gdy klient już nagrywa N+1) bez spekulacji, którą trzeba cofać? Co jest bezpieczne, a co wymaga weryfikacji?
3. Czy `vkWaitSemaphores` na wartość timeline semafora, którą dopiero zgłosi późniejsze `vkQueueSubmit`, można obsłużyć lokalnie lub zwolnić wcześniej? Jak weryfikować?
4. Ile z technik WiCi (trace replay, predykcja odpowiedzi) można zastosować do vkd3d-proton, w którym ok. 1000 rysowań na klatkę ma powtarzalną strukturę?
5. Co jeszcze zmniejsza bajty (nie powtarzać: zstd, prefiks, delta XOR, cache po zawartości)?
Odpowiadaj konkretnie: pomysł, dlaczego zadziała, jak zweryfikować poprawność, ryzyko, oczekiwany zysk w ms na klatkę.
