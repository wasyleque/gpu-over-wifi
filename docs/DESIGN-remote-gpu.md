# Projekt: własny stos zdalnego GPU (gra lokalnie, karta zdalna)

Status: obowiązujący. Zastępuje ścieżkę "Venus przez sieć" i streaming (oba odrzucone: patrz AGENTS.md).

## 1. Cel i kryterium sukcesu (z transkrypcji NetworkChuck / WiCi One)
Aplikacja (gra) działa na laptopie; widzi lokalny, "prawdziwy" GPU; renderuje na karcie serwera przez sieć. Mierzalne cele przeniesione z filmu:
- ruch poleceń: z ~8 GB/s (surowo) do ≤ ~400 Mb/s;
- gra AAA (Cyberpunk, przez Proton) płynnie (~90 FPS w filmie przez Wi-Fi 1,5 ms RTT; u nas kabel, RTT 0,55 ms);
- żadnych specjalnych wersji gier; aplikacja nie wie, że GPU jest zdalny.
**Zasada: bez kompromisów w architekturze** — wszystkie cztery triki są obowiązkowe, a streaming gry z serwera nie jest ścieżką zastępczą. Uwaga uczciwości: film jest sponsorowany, a produkt zamknięty — liczby traktujemy jako cel do zmierzenia, nie fakt. Każdy etap kończy się pomiarem, a odchylenie od celu jest raportowane, nie ukrywane.

## 2. Cztery triki z filmu → nasza realizacja
| # | Trik WiCi | Realizacja |
|---|---|---|
| 1 | polecenia grupowane, kompresowane, wysyłane **bez czekania na odpowiedź** | klasyfikacja poleceń (§3): 69% powierzchni API jest asynchroniczne z definicji; strumień jednokierunkowy, partie, kompresja zstd z kontekstem strumieniowym |
| 2 | dane tylko jako **różnice** | pamięć cieniowa (shadow memory) + śledzenie zmian stron; wysyłane tylko zmienione fragmenty, dodatkowo dyferencjowanie względem poprzedniej klatki |
| 3 | odpowiedzi **z wyprzedzeniem** (zna pytania) | migawka statycznych właściwości urządzenia wysyłana raz przy łączeniu (klient odpowiada lokalnie: zero rund-trip); spekulatywne wykonanie dla zapytań dynamicznych |
| 4 | wynik jako **skompresowane wideo** | po stronie serwera kodowanie obrazu prezentacji (sprzętowo), po stronie klienta dekodowanie sprzętowe i wstawienie do łańcucha wymiany lokalnego GPU |
Plus: "fałszywy sterownik GPU" = nasz klient ICD Vulkan (Linux: biblioteka ładowana przez loader; Windows: później).

## 3. Klasyfikacja poleceń (zmierzona na vk.xml, 865 poleceń)
| Klasa | Poleceń | Zachowanie w sieci |
|---|---|---|
| FireAndForget (`void`, brak wyjść) | 398 (46%) | wysłane partiami, bez odpowiedzi |
| DeferredResult (VkResult, brak wyjść) | 114 (13%) | zakładamy sukces; błąd zgłoszony asynchronicznie (utrata urządzenia) |
| OptimisticCreate (`vkCreate*`/`vkAllocate*`) | 84 (10%) | uchwyt nadaje klient, serwer mapuje; bez czekania |
| Query (`vkGet*`/`vkEnumerate*` z wyjściem) | 249 (29%) | cache z migawki; reszta: zapytanie lub spekulacja |
| Sync (`vkWait*`, `vkAcquire*`, status fence…) | 20 (2%) | wymaga stanu serwera; ukrywane przez pipeline klatek |
Uwaga: to liczby **poleceń w API**, nie wywołań na klatkę. Ścieżka gorąca gry to `vkCmd*` (FireAndForget) plus kilka `vkQueueSubmit`/`vkQueuePresent` na klatkę — dlatego realny udział asynchroniczny jest wyższy; zmierzymy to w M2.

## 4. Architektura
```
 gra (Proton/DXVK) ─ Vulkan loader ─ gpu-icd (fałszywy sterownik) ──TCP/zstd──► gpu-renderd ─ Vulkan NVIDIA
        ▲                                    │ pamięć cieniowa, uchwyty klienta        │ replay, mapa uchwytów
        └── lokalny GPU: prezentacja ◄── dekoder VA-API ◄── wideo ◄── kodek sprzętowy ◄┘
```
- `gpu-vkgen` — generator z `vk.xml`: kodery/dekodery wszystkich poleceń i struktur, tabele dyspozycji ICD, kod replay po stronie serwera, metadane klasy poleceń. Własny; nie korzysta z cudzych generatorów.
- `gpu-wire` — ramki, partie, kompresja, priorytety, sterowanie przepływem, tłumaczenie uchwytów.
- `gpu-icd` — biblioteka `cdylib` eksportująca interfejs ICD (`vk_icdGetInstanceProcAddr` itd.); klient.
- `gpu-renderd` — demon serwera: dekoduje strumień i wykonuje na prawdziwym Vulkanie (ładowanym dynamicznie, bez `ash`).
Język: Rust. Zależności tylko MIT/Apache-2.0/BSD (zstd: wybieramy licencję BSD).

## 5. Najtrudniejsze: pamięć współdzielona (zmierzone w PoC-1)
Aplikacje mapują pamięć GPU (`vkMapMemory`) i piszą do niej bezpośrednio. Nie ma tu wywołania do przechwycenia, więc potrzebne jest **śledzenie zapisów**: klient trzyma lokalną kopię cieniową, chroni strony (`mprotect` + obsługa `SIGSEGV`, ew. `userfaultfd`), a w punktach synchronizacji (`vkQueueSubmit`, `vkFlushMappedMemoryRanges`) wysyła tylko zmienione strony. Ta sama zasada działa w drugą stronę dla odczytów (`vkInvalidateMappedMemoryRanges`, wyniki zapytań). PoC-1 pokazał, że gotowy Venus tego nie robi przez sieć (przekazuje deskryptory plików), więc to jest rdzeń własnej pracy.

## 6. Modele błędów
Optymistyczne tworzenie oznacza, że nieudane `vkCreate*` na serwerze (np. brak pamięci) ujawnia się później. Strategia: serwer zgłasza błąd asynchronicznie; klient przechodzi w stan `VK_ERROR_DEVICE_LOST` (zgodny ze specyfikacją Vulkan), a dla popularnych niepowodzeń (pamięć) przeprowadzamy wstępną rezerwację przy tworzeniu urządzenia.

## 7. Etapy i kryteria (każdy kończy się liczbą w AGENTS.md)
- **M0 Fundamenty:** generator parsuje `vk.xml` i klasyfikuje polecenia (zrobione: 865 poleceń); kodery/dekodery wszystkich poleceń z testami okrężnymi (zapis→odczyt) na losowych danych.
- **M1 Enumeracja:** `vulkaninfo` na laptopie przez `gpu-icd` pokazuje RTX 3090 Ti serwera; po połączeniu zero rund-trip (migawka).
- **M2 Trójkąt:** `vkcube` rysuje na zdalnej karcie; obraz wraca surowy; **pomiar bazowy** (MB/s, rund-trip/klatkę) — ile trzeba zaoszczędzić.
- **M3 Pamięć cieniowa i różnice.**
- **M4 Partie i kompresja; tworzenie optymistyczne.**
- **M5 Wideo zamiast surowych klatek** (kodowanie na serwerze, dekodowanie VA-API na laptopie). Wymaga decyzji: Vulkan Video vs NVENC przez CUDA — pomiar opóźnienia obu.
- **M6 Pipeline klatek i spekulacja** (ukrycie rund-trip), pacing klatek.
- **M7 Gra przez Proton/DXVK:** cel ≤ 400 Mb/s i stabilne FPS; porównanie z deklaracjami z filmu.
- **M8 Klient Windows** (osobny projekt: ICD Windows + gra DirectX).
