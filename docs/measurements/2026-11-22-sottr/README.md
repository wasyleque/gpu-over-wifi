# Shadow of the Tomb Raider: Definitive Edition (Epic, Proton-CachyOS, `SOTTR.exe`), desktop Ryzen 5 1600X (2026-11-22)

Wbudowany benchmark gry, preset High (tekstury High, cienie Normal, AO BTAO, LOD Normal), vsync wyłączony według użytkownika.

## Natywnie, GTX 1080 Ti 11 GB (jeden przebieg, bez naszego sterownika)

Zrzut: [`sottr-native-1080ti-result.jpg`](sottr-native-1080ti-result.jpg).

| Wskaźnik | Wynik |
|---|---|
| Klatki | 11 389 |
| **Średni FPS** | **74** |
| FPS (min / maks / średnio / 95%): CPU gra | 24 / 120 / 80 / 60 |
| FPS: CPU render | 67 / 283 / 154 / 117 |
| FPS: GPU | 69 / 122 / 86 / 74 |
| GPU Bound | 41% (przeważa ograniczenie procesorowe, jak w pozostałych grach na tym komputerze) |

## Zdalnie przez nasz sterownik (RTX 3090 Ti na serwerze)

Do zmierzenia: konfiguracja Heroic ustawiona (te same zmienne sterownika co Rise of the Tomb Raider, log `~/gpu-hub-client/icd-sottr.log`, argument gry `-nolauncher`, kopia poprzedniej konfiguracji
`890d9cf396d04922a1559333df419fed.json.bak-przed-icd`).

### Zdalnie przez nasz sterownik, DirectX 11 (DXVK), RTX 3090 Ti (0.3.69)

Zrzut z ustawieniami i wynikiem: [`sottr-remote-dx11-result.jpg`](sottr-remote-dx11-result.jpg). DirectX 12 OFF (patrz niżej, DX12 nie działa), preset High, 1920x1080, pełny ekran, vsync OFF, TAA.

| Wskaźnik | Natywnie 1080 Ti (DX12) | Zdalnie 3090 Ti (DX11) |
|---|---|---|
| Klatki | 11 389 | 3 830 |
| **Średni FPS** | **74** | **25** (plik wyników: 25,5; min 0,0, maks 69,1) |
| FPS: CPU gra | 80 (24-120) | 71 (35-99) |
| FPS: CPU render | 154 (67-283) | **24 (3-59)** |
| FPS: GPU | 86 (69-122) | 36 (11-267) |
| GPU Bound | 41% | **2%** |

Uwaga do porównania: natywny przebieg był w DX12, zdalny w DX11 (DX12 nie działa, niżej), więc nie jest to ta sama ścieżka renderowania; natywnego DX11 nie zmierzono.
Równomierność klatek (plik `frametimes`, 160 s): p50 35,6 ms, p95 64,6, p99 124,6, maks 262,3; 420 klatek > 60 ms z 4092 (10%), 7 > 200 ms. Średnia klatka co 5 s waha się 18-70 ms (scena jest zmienna).
Diagnostyka ICD: 0 ostrzeżeń watchdoga, bez przejścia na LZ4, ruch na łączu 59-167 MB/s (zstd, poziom adaptacyjny), pobrania leniwe 494 po średnio 3,3 ms (1627 ms łącznie, 0 ucieczek), `hamowanie gry: 0 razy` (hamulec klatek bez działania: scena nie przekroczyła limitu klatek w locie), histogram zapytań z obiegiem po sieci (48 853): p50 < 1 ms, p95 < 10 ms, p99 < 50 ms, maks 405,9 ms, przedziały [<1, <2, <5, <10, <20, <50, <100, >=100 ms] = [27934, 2048, 9414, 7148, 1532, 382, 336, 59].
**Wąskie gardło: wątek renderujący klienta, nie GPU.** Czas CPU wątków w sesji (160 s): `dxvk-cs` x2 **142,4 s** (jeden wątek 142,4 s, czyli ok. 89% rdzenia), `SOTTR.exe` x12 234,7 s (jeden wątek 139,7 s), `gpu-icd-frame0/1` po 135 s (kompresja zapisów, razem ok. 1,7 rdzenia), `gpu-icd-video` x6 42,2 s, `cdcMulticore` ok. 260 s. Wątki `dxvk-cs` i renderujący gry są prawie na 100% jednego rdzenia, więc koszt naszego sterownika na każde polecenie nagrywania (kodowanie `vkCmd*` w szybkiej ścieżce) jest tu krytyczny; natywnie DX12 render CPU to 154 FPS (6,5 ms na klatkę), zdalnie DX11 24 FPS (42 ms).

## Luka: DirectX 12 nie działa (zasoby rezerwowane / pamięć rzadka)

W trybie DX12 gra kończy się crashem (`Access violation`) tuż po starcie; log gry (`Shadow of the Tomb Raider.log` w `Documents`): `cdc::PCDX12MemoryManager::AllocTiled: ... CreateReservedResource(...) failed`, wcześniej `CreateGraphicsPipelineState failed with HRESULT 0x80070057`; vkd3d-proton: `Sparse images not supported with format 138, type 1, samples 1, usage 0x7, tiling 0`. Przyczyna: nasz ICD zgłasza brak formatów obrazów rzadkich (`vkGetPhysicalDeviceSparseImageFormatProperties` zwraca 0 właściwości, szablon w `lib.rs`), a vkd3d-proton nie tworzy wtedy zasobów rezerwowanych (tiled resources), których gra wymaga. Wymagane do wsparcia: (1) odpowiedzi serwera na `vkGetPhysicalDeviceSparseImageFormatProperties(2)` i `vkGetImageSparseMemoryRequirements(2)`, (2) `vkQueueBindSparse` z poprawnym tłumaczeniem uchwytów pamięci w zagnieżdżonych tablicach (kodek generowany z vk.xml), (3) zgłaszanie funkcji `sparseResidencyImage2D` itd. zgodnie z urządzeniem serwera. Wcześniej, w tej samej sesji, naprawiono osobny błąd w tej grze: `vkCmdPushDescriptorSet*` wysyłało do serwera śmieciowe wskaźniki nieużywanych pól `VkWriteDescriptorSet` (serwer: "nieznany uchwyt VkBufferView", a po nim lawina błędów, bo przerywa dekodowanie całej partii; klient zawieszał się na blokadzie instancji, czarny ekran bez ruchu w sieci).
Pozostałe incydenty tej sesji: sprzątanie sesji potrafi utknąć w `vkDestroyDevice` sterownika NVIDIA (100% CPU, serwer niedostępny dla nowej sesji); blokada cyklu życia urządzeń (0.3.55) miała przez to niekończące się czekanie, teraz ma limit 8 s.
