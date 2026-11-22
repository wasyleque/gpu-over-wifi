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

### Wsparcie pamięci rzadkiej (sparse) — pierwszy etap (0.3.70)

Zmiany: (1) `vkGetPhysicalDeviceSparseImageFormatProperties` składane z wariantu `...2` przekazywanego do serwera (`gpu-icd/src/lib.rs`), zamiast stubu zwracającego 0; (2) kodek (`gpu-vkcodec/src/codec.rs`, `read_uint`) potrafi odczytać długość tablicy z parametru-wskaźnika na licznik w trybie wejście/wyjście (wcześniej `BadLen("nieznany identyfikator")` w `vkGetImageSparseMemoryRequirements`; dotyczy wszystkich poleceń z dwukrokowym wzorcem licznik/tablica obsługiwanych generycznie).
Test na prawdziwym urządzeniu (`tools/vk-sparse-formats.py`, `tools/vk-sparse-bind.py`, uruchamiane `tools/run-vk-test.sh <skrypt>` na kliencie, `ctypes` przez nasz ICD, RTX 3090 Ti na serwerze): funkcje `sparseBinding`, `sparseResidencyBuffer/Image2D/Image3D`, `sparseResidency2/4/8/16Samples`, `sparseResidencyAliased` = 1; formaty rzadkie RGBA8 -> kafelek 128x128x1, format 138 -> 256x256x1 (przed zmianą 0 właściwości dla każdego formatu); bufor rzadki 4 MiB: `vkCreateBuffer` (flaga SPARSE_BINDING), `vkGetBufferMemoryRequirements`, `vkAllocateMemory`, `vkQueueBindSparse` (powiązanie i odwiązanie), `vkQueueWaitIdle` = `VK_SUCCESS`; obraz rzadki 1024x1024 RGBA8 (SPARSE_BINDING | SPARSE_RESIDENCY): `vkGetImageSparseMemoryRequirements` (aspekt 0x1, granularność 128x128x1, mip tail od poziomu 1, offset 4194304), `vkQueueBindSparse` jednego kafelka obrazu = `VK_SUCCESS`. Cyberpunk po zmianie: 31,07 FPS (bez regresji).
Niewiadome: czy to wystarcza vkd3d-proton i grze (DX12 Shadow of the Tomb Raider może wymagać jeszcze np. `vkGetImageSparseMemoryRequirements2`, aliasowania pamięci, mip taila); do sprawdzenia przebiegiem gry.

### DirectX 12 działa (0.3.70 + cache zapytań o pamięć rzadką, 0.3.71)

Po wsparciu pamięci rzadkiej (patrz wyżej) Shadow of the Tomb Raider w trybie DirectX 12 uruchamia się i przechodzi benchmark (1080p, High, vsync off, ta sama konfiguracja co natywnie 74 FPS i zdalnie DX11 25 FPS).

| Przebieg | Średnie FPS | p50 / p95 / p99 klatki | Klatki > 60 ms | Klatki > 200 ms | Maks. klatka | Zapytania z obiegiem (p99 / maks) |
|---|---|---|---|---|---|---|
| DX12, bez cache zapytań rzadkich | 35,1 | 25,6 / 43,8 / 58,0 ms | 55 | 21 | 955 ms | >100 ms / 939 ms |
| DX12, z cache zapytań rzadkich (0.3.71) | **38,2** | 24,9 / 41,7 / 49,2 ms | 19 | 0 | 103 ms | <20 ms / 374 ms |

Zmiana: `vkGetPhysicalDeviceSparseImageFormatProperties2` i `vkGetImageSparseMemoryRequirements(2)` dopisane do cache zapytań (`query_cacheable`). Liczba zapytań z obiegiem po sieci spadła z 169 tys. do 84 tys., a czas jednego `GetImageSparseMemoryRequirements` z 3,3 do 1,4 ms. Przestoje 300-950 ms po załadowaniu drugiej sceny zniknęły. Nadal wolne: `WaitSemaphores` (94 tys. po 3,4 ms), `GetDescriptorEXT`; `GetShaderModuleIdentifierEXT` pojawił się tylko w pierwszym przebiegu (67 tys. zapytań) i do zbadania. To jeden przebieg na wariant.

### Synteza deskryptorów tekseli: 5 nowych formatów (0.3.72)

Najczęstsze chybienia syntezy `vkGetDescriptorEXT` w SOTTR DX12 to bufory tekseli formatów 101 (R32G32_UINT) i 107 (R32G32B32A32_UINT), których nie było w tabeli. Sonda na RTX 3090 Ti (`descriptor_probe`, dane: `docs/measurements/descriptor-probes/texel-buffer-rtx3090ti-580-more-formats.txt`) dała wzory `adres >> k`, `zakres >> k`, stała: 99 (k=2, 4115), 101 (3, 3210), 102 (3, 3437), 107 (4, 2777), 108 (4, 2957), zgodne na wszystkich 56 próbkach każdego formatu. Format 104 (12-bajtowy teksel) zostaje na serwerze.

| Przebieg (DX12, 1080p, High, vsync off) | Średnie FPS | p50 / p95 / p99 | Klatki > 60 ms | Maks. | Zapytania z obiegiem | `GetDescriptorEXT` do serwera |
|---|---|---|---|---|---|---|
| 0.3.71 | 38,2 | 24,9 / 41,7 / 49,2 ms | 19 | 103 ms | 84 tys. | 57 tys. po 1,8 ms |
| 0.3.72 | **42,8** | 21,4 / 37,0 / 43,2 ms | 14 | 146 ms | 50 tys. | 23 tys. po 2,3 ms |

Jeden przebieg na wariant (część poprawy może być rozrzutem; liczba zapytań do serwera jest jednak zmierzona wprost). Natywnie na GTX 1080 Ti: 74 FPS.

### Profil CPU klienta (perf, 15 s w benchmarku, DX12, 0.3.72)

`perf record -F 499 --call-graph dwarf` na `SOTTR.exe` (desktop, Ryzen 5 1600X). Udział w próbkach: kod gry (Wine PE, `[JIT]`) 45,8%, jądro 21,6%, `libgpu_icd_pipelined.so` 15,9%, libc 8,9%, d3d12core 2,6%. Wg wątków: `cdcMulticore Lo` 25,4%, `SOTTR.exe` 18,6%, `cdcMulticore Id` 15,9%, `cdcMulticore St` 14,3%; wątki ICD: kompresja `gpu-icd-frame0/1` po ok. 4%, wideo 3,4%. Największa pozycja jądra: `pagemap_scan_pmd_entry` 2,5% (śledzenie zmian pamięci), potem blokady `native_queued_spin_lock_slowpath` ~1%.
Wniosek: sam ICD to ok. jedna szósta CPU klienta; kod gry dominuje. Luka do natywnych 74 FPS (ta sama maszyna) nie wynika więc głównie z kosztu CPU samego ICD, tylko z opóźnień i szeregowania (czekanie na GPU, `WaitSemaphores`, odczyty), do zbadania osobno. Jeden próbkowany odcinek.

### Które wątki czekają na serwer (instrumentacja `zablokowane wątki`, 0.3.74)

Nowa linia logu (`GPU_ICD_STATS=periodic`): czas zablokowania wątków w poleceniach z obiegiem po sieci, wg (wątek, polecenie). Przebieg DX12, czysty pomiar: **44,4 FPS**. Suma z ok. 170 s:

| Wątek / polecenie | Wywołań | Suma | Uwagi |
|---|---|---|---|
| vkd3d_fence / WaitSemaphores | 144 tys. | 229 s | wątek pomocniczy, czeka na GPU |
| vkd3d_queue / WaitSemaphores | 31 tys. | 129 s | wątek pomocniczy |
| vkd3d-swapchain / WaitSemaphores | 13 tys. | 51 s | |
| ResourceCreatio / GetImageSparseMemoryRequirements | 23 tys. | 38 s | ładowanie zasobów |
| ResourceCreatio / GetDescriptorEXT | 17,5 tys. | 37 s | |
| SOTTR.exe / GetDescriptorEXT | 5,3 tys. | 14,8 s | wątek główny gry, ok. 9% czasu |
| cdcMulticore St / WaitSemaphores | 4 tys. | 11,3 s | wątek renderu gry, ok. 7% |
| cdcMulticore Lo / GetDescriptorEXT | 318 | 1,4 s | |

Wątki renderu gry (`cdcMulticore *`, `SOTTR.exe`) blokują się w zapytaniach tylko ok. 7-9% czasu; wielkie sumy to wątki pomocnicze vkd3d czekające na GPU (czekanie z natury). Hamulec klatek: 736 ms łącznie w 170 s, czyli nie ogranicza. Z profilu `perf`: koszt ICD w wątkach gry to ok. 10% ich CPU (`forward_inner`, `enc_struct`, `find_struct`, `read_uint`), kod gry ok. 60%, jądro ok. 18%.
Wniosek: ani zapytania synchroniczne, ani koszt kodowania nie tłumaczą luki do natywnych 74 FPS (łącznie górna granica ok. 15-20%). Hipoteza do sprawdzenia: opóźnienie wykonania klatki na serwerze (od `vkQueueSubmit` do sygnału fence) w połączeniu z limitem klatek w locie samej gry (vkd3d czeka na fence klatki N-2).
