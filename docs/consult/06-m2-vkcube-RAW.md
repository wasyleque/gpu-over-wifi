# Konsultacja 06: M2 vkcube (DeepSeek Flash na .2, 2026-11-01)

STATUS: surowa odpowiedź, jeszcze NIEPRZEJRZANA przez Claude (brak limitu sesji). Przed użyciem sprawdzić krytycznie: poprzednie odpowiedzi miały błędy (offsety zamiast adresów w pNext, dlclose w Drop).

---

# M2: vkcube na zdalnej karcie — projekt ścieżki docelowej

Poniżej decyzje architektoniczne dla M2. Wszystko, co tu ustalimy, ma przetrwać do M7 bez przepisywania — zmieniają się tylko: kodek (M5), dyferencjowanie pamięci (M3) i pipeline klatek (M6).

## 1. Funkcje Vulkana wywoływane przez vkcube (kolejność i klasa)

Wariant `vkcube --c N` (offscreen, `VK_EXT_headless_surface` lub brak surface) — kolejność z `cube.c`:

**Start (raz):**
- `vkEnumerateInstanceExtensionProperties`, `vkEnumerateInstanceLayerProperties` — **Query**, lokalnie (statyczna migawka z M1).
- `vkCreateInstance` — **OptimisticCreate**, lokalnie (ICD tworzy własny obiekt, serwer nie musi wiedzieć; wyjątek: gdy `pApplicationInfo->apiVersion` > nasza — walidacja lokalna).
- `vkEnumeratePhysicalDevices` — **Query**, lokalnie z cache (M1).
- `vkGetPhysicalDeviceProperties/Features/QueueFamilyProperties/MemoryProperties/FormatProperties/ImageFormatProperties` — **Query**, lokalnie z migawki. **Zero serwera.**
- `vkEnumerateDeviceExtensionProperties` — **Query**, lokalnie z migawki (musimy filtrować: nie reklamujemy `VK_KHR_display` itp., patrz §7).
- `vkCreateDevice` — **OptimisticCreate**, **musi iść do serwera** (potrzebujemy prawdziwego `VkDevice` i kolejek). Synchroniczne w M2 (jednorazowo, RTT 0,2 ms nieistotne).
- `vkGetDeviceQueue` — **Query**, lokalnie (indeks kolejki z migawki).
- `vkCreateSwapchainKHR` (wariant z oknem) — **OptimisticCreate**, do serwera; w M2 offscreen pomijamy.
- `vkGetSwapchainImagesKHR` — **Query**, do serwera (lista uchwytów obrazów); w M2 offscreen: `vkCreateImage` × N.

**Pętla klatki (na klatkę):**
- `vkCreateBuffer`, `vkCreateImage`, `vkCreateImageView`, `vkCreateRenderPass`, `vkCreateFramebuffer`, `vkCreateShaderModule`, `vkCreatePipelineLayout`, `vkCreateGraphicsPipelines`, `vkCreateCommandPool`, `vkCreateDescriptorSetLayout`, `vkCreateDescriptorPool`, `vkAllocateDescriptorSets`, `vkCreateSemaphore`, `vkCreateFence` — **OptimisticCreate**, do serwera, bez czekania (uchwyty klienta).
- `vkAllocateMemory`, `vkBindBufferMemory`, `vkBindImageMemory` — **OptimisticCreate**, do serwera.
- `vkMapMemory`, `vkUnmapMemory`, `vkFlushMappedMemoryRanges` — **lokalnie + synchronizacja** (patrz §3). vkcube mapuje staging buffer i pisze wierzchołki.
- `vkBeginCommandBuffer`, `vkCmdBeginRenderPass`, `vkCmdBindPipeline`, `vkCmdBindVertexBuffers`, `vkCmdBindDescriptorSets`, `vkCmdDraw`, `vkCmdEndRenderPass`, `vkCmdCopyImageToBuffer` (nasz readback), `vkEndCommandBuffer` — **FireAndForget**, do serwera w partii.
- `vkQueueSubmit` — **DeferredResult** (zwraca `VK_SUCCESS` lokalnie), do serwera; **punkt synchronizacji pamięci** (flush brudnych stron) i **punkt bariery** (patrz §4).
- `vkWaitForFences` — **Sync**, **musi być synchroniczne** (czekamy na wynik serwera). W M2: blokujące; w M6: ukryte przez pipeline klatek.
- `vkResetFences`, `vkResetCommandBuffer` — **FireAndForget** (reset fence lokalnie, bo fence to nasz obiekt).
- `vkAcquireNextImageKHR` — **Sync** w wersji z oknem; w M2 offscreen nieużywane.
- `vkQueuePresentKHR` — **DeferredResult**, do serwera; w M2 offscreen: nieużywane, zamiast tego readback.
- `vkDeviceWaitIdle` — **Sync**, do serwera (na koniec).
- `vkDestroy*`, `vkFreeMemory`, `vkFreeCommandBuffers` — **FireAndForget**, do serwera; uchwyty zwalniane lokalnie natychmiast.

**Wniosek dla M2:** cała gorąca ścieżka to `vkCmd*` (FireAndForget) + `vkQueueSubmit` + `vkWaitForFences`. Jedyny obowiązkowy RTT na klatkę to `vkWaitForFences`. To jest liczba, którą M6 musi ukryć.

## 2. Kształt protokołu M2

**Jeden strumień poleceń w partiach, nie osobne żądania.** Uzasadnienie: `vkCmd*` są z definicji FireAndForget i muszą lecieć razem z `vkQueueSubmit`; osobne żądania na każde polecenie dają 200+ RTT na klatkę i zabijają cel. Protokół z M1 (jedno żądanie → jedna odpowiedź) **zostaje dla kanału kontrolnego** (Query, Sync, CreateDevice), a **dochodzi kanał strumieniowy** dla FireAndForget/DeferredResult/OptimisticCreate.

Dwa kanały w jednym TCP (lub dwa TCP — prostsze w M2, do decyzji przy M4):
- **Kanał A (kontrolny, request/response):** Query, Sync, `vkCreateDevice`. Format z M1 bez zmian.
- **Kanał B (strumień, jednokierunkowy):** partie poleceń, bez odpowiedzi. Serwer wysyła tylko asynchroniczne błędy (patrz niżej) i ewentualne „ack" na granicy partii (opcjonalne, do pomiaru).

**Partia** = `[u32 len][u32 seq][u32 crc32c][payload]`. Payload: ciąg `[u16 opcode][args...]`. Opcode = indeks polecenia w tabeli wygenerowanej z `vk.xml` (już mamy). Partia kończy się, gdy: (a) `vkQueueSubmit`/`vkQueuePresentKHR` (granica semantyczna), (b) rozmiar > 64 KiB, (c) minęło 1 ms od pierwszej komendy (timer). To daje grupowanie (trik 1) bez czekania.

**Uchwyty:** klient przydziela `u64` z własnej przestrzeni: `[16 bitów rodzaj][48 bitów licznik]`. Rodzaj = `VkObjectType` (mamy z vk.xml). Licznik globalny, nigdy nie recyklujemy w obrębie sesji — **kolizje niemożliwe** (48 bitów wystarczy na 2^48 obiektów; sesja żyje godziny). Serwer trzyma `HashMap<u64, RealHandle>` + `HashMap<RealHandle, u64>` (do mapowania zwrotnego, np. gdy serwer zwraca uchwyt w strukturze wyjściowej — w M2 nie występuje, ale tablica jest). Zwalnianie: `vkDestroy*`/`vkFree*` w strumieniu B usuwa wpis po stronie serwera; klient usuwa natychmiast. **Nie czekamy na potwierdzenie** — jeśli serwer nie znajdzie uchwytu, to błąd programisty (panika w debug, `DEVICE_LOST` w release).

**Błędy asynchroniczne:** serwer, gdy `vkCreate*`/`vkAllocate*` zwróci błąd, wysyła na kanale A ramkę `ASYNC_ERROR{opcode, client_handle, VkResult}`. Klient ustawia globalny `AtomicBool device_lost` i od tej chory wszystkie `vkCreate*` zwracają `VK_ERROR_DEVICE_LOST`, a `vkQueueSubmit` — `VK_ERROR_DEVICE_LOST`. To zgodne ze specyfikacją (utrata urządzenia jest legalna w dowolnym momencie). W M2 nie implementujemy odzyskiwania — tylko raportowanie.

**Kodowanie struktur ze wskaźnikami — użycie generatora, nie ręczne 80 funkcji.** Mamy już w `gpu-vkgen`: układ struktur, `has_output_pointers`, łańcuchy pNext. Rozszerzamy generator o **jedną funkcję na strukturę**: `encode_<Struct>(&Struct, &mut Writer, &Ctx)` i `decode_<Struct>(&mut Reader, &Ctx) -> Struct`, gdzie `Ctx` niesie mapę uchwytów klienta i alokator tymczasowy. Zasady generowania:
- Wskaźnik na strukturę (`pNext`, `pApplicationInfo`) → **inline** w strumieniu (rekurencyjnie), nie adres. `pNext` kodujemy jako listę `[u32 sType][payload]...[0]` — serwer odtwarza łańcuch z tablicy sType (już mamy 1276 struktur).
- Wskaźnik na tablicę (`pCode`, `pVertexData`) → `[u32 count][elementy]`. Dla `void*` z `len` w innym polu (`pCodeSize`) — generator czyta pole-rozmiar z tej samej struktury (mamy to w vk.xml jako `len` attribute).
- Wskaźnik wyjściowy (`pProperties`, `pCount`) → **nie kodujemy danych**; kodujemy tylko „slot", a serwer wypełnia i odsyła w odpowiedzi kanału A. Dla `vkEnumerate*` z `pCount` — dwufazowo: najpierw `pCount` (Query), potem dane.
- Uchwyty (`VkDevice`, `VkBuffer`, …) → `u64` klienta; serwer tłumaczy przez mapę.
- `VkAllocationCallbacks*` → **zawsze NULL** w strumieniu (klient alokuje lokalnie, serwer używa domyślnych). To upraszcza i jest zgodne z praktyką.
- `VkPhysicalDevice` → specjalny uchwyt (jeden, z M1), serwer mapuje na prawdziwy.

Generator generuje też **tabelę dyspozycji ICD** (`vk_icdGetInstanceProcAddr` → funkcja) i **szkielety** `vkCmd*` (FireAndForget: zakoduj i wrzuć do partii) vs `vkCreate*` (OptimisticCreate: przydziel uchwyt, zakoduj, wrzuć do partii) vs Query/Sync (kanał A). Ręcznie piszemy tylko ~15 funkcji specjalnych (`vkCreateInstance`, `vkCreateDevice`, `vkAllocateMemory`, `vkMapMemory`, `vkQueueSubmit`, `vkWaitForFences`, `vkAcquireNextImageKHR`, `vkQueuePresentKHR`, `vkGetDeviceProcAddr`, `vk_icdGetInstanceProcAddr`, `vk_icdNegotiateLoaderICDInterfaceVersion`, readback). Reszta — generowana.

## 3. Pamięć (M2 prosta, M3 bez przepisywania)

**Model:** klient alokuje **lokalny** `memfd` (lub `mmap` anonimowy) o rozmiarze `VkMemoryAllocateInfo::allocationSize` i zwraca go aplikacji z `vkMapMemory`. Serwer alokuje prawdziwą `VkDeviceMemory` tego samego rozmiaru. **Dwie kopie, synchronizacja w punktach.**

- `vkAllocateMemory` — OptimisticCreate: klient tworzy `memfd`, wysyła `{client_handle, size, memoryTypeIndex}` do serwera; serwer robi `vkAllocateMemory`. Uchwyt pamięci = u64 klienta.
- `vkMapMemory` — **lokalnie**, zwraca `mmap(memfd)`. Zero serwera. (Wyjątek: `VK_MEMORY_PROPERTY_HOST_COHERENT_BIT` — i tak synchronizujemy przy Flush/Submit.)
- `vkUnmapMemory` — lokalnie `munmap`.
- `vkFlushMappedMemoryRanges` — **punkt synchronizacji**: wysyła zakresy `{offset, size}` na kanale B jako polecenie specjalne `SYNC_WRITE{memory_handle, offset, size}`; serwer robi `memcpy` z bufora odebranego w strumieniu do swojej `VkDeviceMemory` (przez `vkMapMemory` serwera raz, trzymany). W M2: **całe zakresy**, bez różnic.
- `vkQueueSubmit` — przed wysłaniem partii: dla wszystkich zmapowanych pamięci z brudnymi stronami (w M2: wszystkie zmapowane) wyślij `SYNC_WRITE` całych zakresów. To jest miejsce, w które M3 wstawia **tylko brudne strony** (userfaultfd) — interfejs `SYNC_WRITE{handle, offset, size}` **nie zmienia się**, zmienia się tylko to, co klient wybiera do wysłania. Dlatego M3 nie przepisuje M2.
- `vkInvalidateMappedMemoryRanges` — w M2: no-op (dane z serwera nie wracają do pamięci gościa poza readbackiem). W M3: `SYNC_READ` z serwera.
- Readback (patrz §4) — osobna ścieżka, nie przez `VkDeviceMemory` gościa.

**Kluczowa decyzja dla M3:** `memfd` klienta jest **źródłem prawdy** dla pamięci gościa. Serwer ma kopię. W M3 `userfaultfd` na `memfd` (tryb `UFFDIO_REGISTER_MODE_WP`) daje listę brudnych stron — wysyłamy tylko je. Interfejs `SYNC_WRITE` bez zmian. **Nie** robimy pamięci współdzielonej przez sieć (to ślepa uliczka z PoC-1).

## 4. Powrót obrazu (M2 surowy, M5 tylko kodek)

**M2 offscreen:** po `vkQueueSubmit` z `vkCmdCopyImageToBuffer` (obraz → staging buffer serwera), serwer robi `vkMapMemory` staging buffer, wysyła surowe bajty na kanale A jako odpowiedź na `vkWaitForFences` (lub osobne `READBACK{handle, w, h, format, bytes}`). Klient odbiera i **wstawia do lokalnego swapchaina** (Intel) przez `vkCmdCopyBufferToImage` na lokalnym urządzeniu — albo, prościej w M2, zapisuje do pliku PPM i mierzy. **Wariant z oknem:** `vkQueuePresentKHR` → serwer robi readback, wysyła, klient prezentuje lokalnie.

**Kształt dla M5:** definiujemy **jeden typ ramki wideo**:
```
Frame { seq: u32, w: u16, h: u16, format: PixelFormat, payload: Bytes }
```
gdzie `PixelFormat` = `RawRGBA8` (M2) | `HevcNvenc` (M5). Serwer ma trait `Encoder { fn encode(&mut self, image) -> Frame }`; M2 implementuje `RawEncoder` (memcpy), M5 `NvencEncoder`. Klient ma trait `Decoder { fn decode(&mut self, frame) -> LocalImage }`; M2 `RawDecoder`, M5 `VaapiDecoder`. **Nic więcej się nie zmienia** — `vkQueuePresentKHR`/readback woła `Encoder`, klient `Decoder`, potem `vkCmdCopyBufferToImage` na Intelu. To jest cała abstrakcja M5.

**`vkAcquireNextImageKHR`/`vkQueuePresentKHR` w M2 (wariant z oknem):**
- `vkAcquireNextImageKHR`: klient zwraca **lokalny** indeks obrazu (cyklicznie 0..N-1), sygnalizuje lokalny semafor (jeśli podany) natychmiast. Serwer nie wie o acquire — obrazy swapchaina serwera są tylko celami renderowania, a prezentacja to readback. To łamie semantykę „obraz jest gotowy", ale w M2 akceptowalne (vkcube nie czyta obrazu przed prezentacją). W M6: acquire staje się punktem pipeline'u.
- `vkQueuePresentKHR`: DeferredResult, wysyła `PRESENT{swapchain, image_index}`; serwer robi readback i wysyła `Frame`. Klient dekoduje i prezentuje na Intelu. **Zwraca `VK_SUCCESS` natychmiast** — opóźnienie prezentacji ukryte przez pipeline (M6).

## 5. Plan pomiaru bazowego (M2)

**Co liczyć, na klatkę:**
- bajty wysłane wg klasy: `cmd_bytes` (kanał B, FireAndForget), `create_bytes` (OptimisticCreate), `query_bytes` + `sync_bytes` (kanał A), `mem_bytes` (`SYNC_WRITE`), `video_bytes` (readback);
- rund-tripy: liczba żądań kanału A na klatkę (oczekiwane: 1 — `vkWaitForFences`; jeśli > 1, coś przecieka);
- czas: `t_frame` (od `vkQueueSubmit` do odebrania `Frame`), `t_encode`, `t_network`, `t_decode`, `t_present`;
- FPS.

**Gdzie liczniki:** w `gpu-wire` — każda ramka partii ma `seq` i `class`; licznik per klasa w `AtomicU64`. W `gpu-icd` — `Instant::now()` wokół `vkQueueSubmit` i `vkWaitForFences`. W `gpu-renderd` — czas `vkQueueSubmit` serwera i readback. Eksport: `eprintln!` co sekundę + plik CSV (`--metrics out.csv`).

**Progi potwierdzające opłacalność trików (na podstawie celu 400 Mb/s):**
- `cmd_bytes` < 50 KB/klatkę (po kompresji) — inaczej trik 1 nie działa;
- `mem_bytes` < 100 KB/klatkę po M3 — inaczej trik 2 nie działa;
- rund-tripy ≤ 1/klatkę po M6 — inaczej trik 3 nie działa;
- `video_bytes` < 200 KB/klatkę przy 1080p60 — inaczej trik 4 nie działa (surowe 1080p RGBA = 8 MB/klatkę, 60 FPS = 4 Gb/s; HEVC ~10 Mb/s = 1,25 MB/klatkę, czyli 100 Mb/s — mieści się).
- **Baza M2 (surowe):** oczekuję ~8 MB/klatkę readbacku + ~kilka KB poleceń. To jest liczba, od której odejmujemy.

## 6. Podział na moduły (kolejność, zlecalność)

1. **`gpu-vkgen`: rozszerzenie o kodery/dekodery struktur** — **za trudne dla małego modelu** (generator, makra, `unsafe` w dekoderze). Ja.
2. **`gpu-wire`: kanał B (partie, opcode, CRC), `SYNC_WRITE`, `ASYNC_ERROR`, `Frame`** — mały model z testami okrężnymi (zapis→odczyt losowych partii). Średnie.
3. **`gpu-renderd`: tabela dyspozycji z generatora, replay `vkCmd*`, mapa uchwytów, `SYNC_WRITE` przez `memcpy`** — **za trudne** (dynamiczne ładowanie Vulkan, `unsafe`, mapy). Ja.
4. **`gpu-icd`: szkielety `vkCmd*` (generowane), `vkCreate*` (OptimisticCreate), `vkAllocateMemory`/`vkMapMemory` (memfd)** — średnie; mały model z gotowymi szkieletami z generatora.
5. **`gpu-icd`: `vkQueueSubmit`/`vkWaitForFences`/`vkAcquireNextImageKHR`/`vkQueuePresentKHR`** — **za trudne** (synchronizacja, semafory, pipeline). Ja.
6. **Readback + `RawEncoder`/`RawDecoder` + prezentacja na Intelu** — średnie; mały model z testem „PPM się zgadza".
7. **Liczniki i CSV** — łatwe, mały model.
8. **Test integracyjny: `vkcube --c 10` → 10 klatek, PPM, CSV** — łatwe.

Kolejność: 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8. Każdy moduł ma test: 2 — roundtrip; 4 — `vulkaninfo` dalej działa; 6 — PPM; 8 — całość.

## 7. Pułapki (co najczęściej wywraca zdalne ICD)

- **`vkGetDeviceProcAddr` musi zwracać nasze funkcje, nie `NULL`.** vkcube sprawdza `vkGetDeviceProcAddr(device, "vkCmdDraw")` — jeśli `NULL`, panika. Generator musi wypełnić tabelę dla **wszystkich** 865 poleceń (nawet jeśli część to stub `VK_ERROR_FEATURE_NOT_PRESENT`).
- **`pNext` w `VkDeviceCreateInfo`:** vkcube dodaje `VkPhysicalDeviceFeatures2`/`VkPhysicalDeviceVulkan12Features`. Jeśli nie przekażemy łańcucha do serwera, `vkCreateDevice` serwera odrzuci funkcje. Generator musi kodować `pNext` rekurencyjnie — **to jest najczęstszy błąd**.
- **`VK_KHR_swapchain` na ICD bez okna:** nie reklamujemy rozszerzenia, jeśli nie mamy surface. vkcube z `--c` używa headless; wariant z oknem wymaga `VK_KHR_xcb_surface` — **reklamujemy tylko to, co serwer ma** (migawka z M1). Inaczej `vkCreateSwapchainKHR` na serwerze padnie.
- **Ukryte zależności od lokalnego GPU:** vkcube ładuje `libvulkan.so.1` i robi `dlopen` — nasz ICD musi być **jedynym** widocznym sterownikiem (`VK_DRIVER_FILES`), inaczej loader może wybrać Intel i pomylić `Vk