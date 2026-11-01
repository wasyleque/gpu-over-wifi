> **Źródło: DeepSeek V4.1 Flash (lokalnie, `.2`), konsultacja część 2, 2026-11-01. Nieweryfikowane. Odpowiedź ucięta limitem tokenów przy końcu §3.16.**
> **Uwagi recenzji (do uwzględnienia przy implementacji):**
> 1. Śledzenie zapisów przez `mprotect`+`SIGSEGV` kosztuje µs/stronę; gra wgrywająca dziesiątki MB/klatkę da tysiące błędów stron. Handler musi być async-signal-safe (bitmapa atomowa, bez alokacji i blokad). Zmierzyć w M3 i porównać z `userfaultfd` WP oraz haszowaniem.
> 2. Serwer nie ma systemu okien: swapchain jest wirtualny po obu stronach; tempo klatek ustala powrót wideo (backpressure), nie `vkAcquireNextImageKHR` na serwerze.
> 3. Lokalny model swapchaina na kliencie musi pilnować maksymalnej liczby klatek w locie (limit przed oczekiwaniem na powrót wideo).

# Sekcja 3 — Tabela trudnych przypadków Vulkana (decyzje)

Konwencja: **RT** = rund-trip do serwera. „Klatka” = jeden `vkQueuePresentKHR`. Wszystkie liczby RT to *ścieżka gorąca*; ścieżki zimne (init, tworzenie zasobów) mogą mieć RT, byle nie w pętli klatki.

---

## 3.1 `vkMapMemory` / `vkUnmapMemory`

**(a)** async (lokalnie synchroniczne, sieciowo fire-and-forget).
**(b)** Klient alokuje lokalne `memfd` (lub `MAP_SHARED` na `memfd`) o rozmiarze `allocationSize` i zwraca jego adres z `vkMapMemory`. Serwer dostaje `MemAlloc{wire_id, size, mem_type_index}` przy `vkAllocateMemory` i trzyma własne `VkDeviceMemory` + `vkMapMemory` na czas życia. Mapowanie po stronie serwera jest *stałe* (map raz przy alokacji, unmap przy `vkFreeMemory`), niezależnie od tego, co robi klient — to eliminuje RT na map/unmap.
**(c)** 0 RT. `vkMapMemory` = czysto lokalna operacja na cieniowanym `memfd`.
**(d)** Najgorszy przypadek: aplikacja mapuje całą pamięć HOST_VISIBLE i pisze w losowych miejscach — śledzenie stron musi objąć cały zakres. Ograniczenie: `mprotect(PROT_READ)` na całym mapowaniu + `SIGSEGV` handler; strony brudzone wchodzą do kolejki `dirty_pages` (bitmapa 4 KiB). Dla `DEVICE_LOCAL` bez `HOST_VISIBLE` mapowanie i tak jest zabronione przez specyfikację — nie dotyczy.
**(e)** Pułapka: `vkMapMemory` może być wywołane wielokrotnie na tej samej alokacji z różnymi offsetami/rozmiarami (`VkMappedMemoryRange` w `vkFlush*`). Naiwna implementacja „jedno mapowanie = jeden bufor” gubi drugi offset. Trzeba trzymać listę `(offset, size, ptr)` per `VkDeviceMemory` i sumować dirty pages w przestrzeni alokacji, nie mapowania. Druga pułapka: `VK_MEMORY_PROPERTY_HOST_COHERENT_BIT` — jeśli ustawione, aplikacja *nie* wywoła `vkFlush*`; musimy wtedy wysyłać dirty pages przy każdym `vkQueueSubmit` (patrz 3.3), bo inaczej dane nie dotrą.

---

## 3.2 `vkFlushMappedMemoryRanges` / `vkInvalidateMappedMemoryRanges`

**(a)** flush = async (fire-and-forget, ale z barierą kolejności wobec późniejszych `vkQueueSubmit`); invalidate = sync (wymaga danych z serwera).
**(b)** Flush: wysyłamy tylko strony z bitmapy dirty (trik 2). Invalidate: serwer wysyła strony zmienione od ostatniego invalidate — serwer też trzyma bitmapę dirty *dla kierunku serwer→klient* (zapis z GPU przez `vkCmdCopyBuffer`, `vkCmdUpdateBuffer`, `vkCmdFillBuffer`).
**(c)** Flush: 0 RT (wysyłka w partii). Invalidate: 1 RT, chyba że trafimy w okno spekulacji (patrz niżej).
**(d)** Najgorszy: `vkInvalidateMappedMemoryRanges` w pętli klatki na dużym buforze odczytu (np. staging readback). Ograniczenie: serwer wysyła różnice *proaktywnie* po `vkQueueSubmit` z `VK_ACCESS_HOST_READ_BIT` w barierze — klient ma dane gotowe, zanim aplikacja zawoła invalidate. To jest trik 3 w praktyce: serwer „zna pytanie” z góry, bo widzi barierę pamięci.
**(e)** Pułapka: `vkFlushMappedMemoryRanges` nie jest barierą wykonania — aplikacja może po nim pisać dalej do tej samej strony. Naiwne „wyślij i wyczyść bitmapę” gubi zapisy między flush a submit. Rozwiązanie: bitmapa dirty ma dwa poziomy — `flushed` i `dirty`; `vkQueueSubmit` wysyła `dirty \ flushed` i scala. Druga pułapka: `VkMappedMemoryRange` z `size = VK_WHOLE_SIZE` — trzeba rozwinąć do `allocationSize - offset`.

---

## 3.3 `vkQueueSubmit` / `vkQueueSubmit2`

**(a)** async (fire-and-forget), ale to **punkt synchronizacji pamięci cieniowej** — tu domykamy dirty pages i wysyłamy je przed partią poleceń.
**(b)** Cache: stan fence/semaphore po stronie klienta (liczniki timeline, bity binarne). Klient zna semantykę: `VkSubmitInfo` z `pWaitSemaphores`/`pSignalSemaphores` i `VkTimelineSemaphoreSubmitInfo` jest odtwarzany 1:1 na serwerze. Klient utrzymuje *lokalną* kopię wartości timeline (patrz 3.5).
**(c)** 0 RT na submit. RT pojawia się tylko przy `vkWait*` (3.5) i przy `vkAcquireNextImageKHR` (3.6).
**(d)** Najgorszy: gra submituje 1–3 razy na klatkę, ale z wieloma `VkCommandBuffer` i semaforami timeline. Ograniczenie: cała paczka `MemDiff + Cmd[] + Submit` idzie jedną ramką z `last-in-batch`; serwer wykonuje atomowo. Drugi najgorszy: `vkQueueSubmit2` z `VkSubmitInfo2` i `VkSemaphoreSubmitInfo` — generator musi obsłużyć oba warianty (różne struktury, ta sama semantyka).
**(e)** Pułapka 1: **kolejność** — jeśli wyślemy `MemDiff` po `Cmd[]`, serwer wykona polecenia na starych danych. Ramka musi mieć jawne uporządkowanie: `MemDiff` przed `Cmd[]` w payloadzie, a serwer przetwarza sekwencyjnie. Pułapka 2: semafory binarne mają stan „signaled/unsignaled” — jeśli klient zgadnie źle (np. po reconnect), serwer zablokuje się na `vkWaitSemaphores`. Rozwiązanie: przy `Hello` klient wysyła pełny stan semaforów; serwer odtwarza. Pułapka 3: `VkTimelineSemaphoreSubmitInfo::pWaitSemaphoreValues` może czekać na wartość, której klient jeszcze nie zna (bo serwer ją nadał) — patrz 3.5.

---

## 3.4 `vkWaitForFences` / `vkGetFenceStatus`

**(a)** sync (wymaga stanu serwera), ale **ukrywane przez pipeline klatek**.
**(b)** Klient trzyma lokalny licznik „submitów” per fence. Serwer wysyła `FenceSignal{wire_id, value}` po wykonaniu `vkQueueSubmit` — asynchronicznie, bez proszenia. Klient w `vkWaitForFences` czeka na lokalny warunek (licznik ≥ wartość), nie na RT.
**(c)** 0 RT w stanie ustalonym. 1 RT tylko przy pierwszym oczekiwaniu na dany fence po reconnect.
**(d)** Najgorszy: gra czeka na fence natychmiast po submit (single-buffered) — wtedy RT = RTT = 0,55 ms na klatkę, co przy 90 FPS (11 ms) jest akceptowalne, ale przy 240 FPS (4 ms) boli. Ograniczenie: pacing — klient opóźnia `vkQueueSubmit` o 1 klatkę (render-ahead), żeby fence zdążył wrócić przed `vkWaitForFences`. To jest trik 3 + trik 4 razem.
**(e)** Pułapka: `vkWaitForFences` z `waitAll = VK_FALSE` — czekamy na *dowolny* fence z listy. Naiwne „czekaj na wszystkie” zawiesza grę. Trzeba czekać na pierwszy sygnał z lokalnego zbioru. Druga pułapka: `timeout = 0` (polling) — musi być lokalny, bez RT; inaczej gra spamuje sieć. Trzecia: `vkResetFences` — musi wyzerować lokalny licznik *i* wysłać reset do serwera, inaczej następny wait czeka na nieistniejący sygnał.

---

## 3.5 Semafory timeline (`vkWaitSemaphores`, `vkSignalSemaphore`, `vkGetSemaphoreCounterValue`)

**(a)** `vkSignalSemaphore` = async; `vkWaitSemaphores` = sync (ukrywane); `vkGetSemaphoreCounterValue` = spekulatywne (lokalna kopia).
**(b)** Klient trzyma `HashMap<wire_id, u64>` z ostatnio znaną wartością. Serwer wysyła `TimelineUpdate{wire_id, value}` po każdym sygnale (z `vkQueueSubmit` lub `vkSignalSemaphore`). Klient aktualizuje kopię.
**(c)** `vkGetSemaphoreCounterValue`: 0 RT (zwraca lokalną kopię). `vkWaitSemaphores`: 0 RT jeśli lokalna kopia ≥ wartość; inaczej 1 RT (lub czekanie na asynchroniczny update).
**(d)** Najgorszy: gra używa timeline do synchronizacji między wątkami renderu i prezentacji (typowe w nowoczesnych silnikach). Jeśli klient zgadnie wartość, którą serwer nadał inaczej (np. po utracie ramki), dojdzie do rozjazdu. Ograniczenie: wartości timeline są **monotoniczne** — klient może bezpiecznie zaokrąglać w górę przy spekulacji, ale nigdy w dół. Przy rozjeździe: reconnect i pełna resynchronizacja.
**(e)** Pułapka: `vkWaitSemaphores` z `VkSemaphoreWaitInfo::flags = VK_SEMAPHORE_WAIT_ANY_BIT` — czekamy na dowolny z listy. Naiwne „czekaj na wszystkie” zawiesza. Druga: `vkGetSemaphoreCounterValue` musi zwracać wartość *widzianą przez klienta*, nie serwera — inaczej gra podejmie decyzję na podstawie stanu, którego jeszcze nie ma lokalnie (use-after-free na zasobach).

---

## 3.6 `vkAcquireNextImageKHR`

**(a)** sync (wymaga stanu swapchaina na serwerze), ale **spekulatywne** — klient zna kolejność obrazów z góry.
**(b)** Klient utrzymuje lokalny model swapchaina: lista `VkImage` (wire_id), indeks następnego obrazu, stan semafora/fence z `VkAcquireNextImageInfoKHR`. Serwer wysyła `SwapchainState{next_index, images_ready[]}` po każdym `vkQueuePresentKHR`.
**(c)** 0 RT w stanie ustalonym (klient zwraca `next_index` z lokalnego modelu). 1 RT przy pierwszym acquire i po zmianie rozmiaru.
**(d)** Najgorszy: `vkAcquireNextImageKHR` z `timeout = UINT64_MAX` i pustym swapchainem (np. okno zminimalizowane) — klient nie może zgadnąć, musi czekać na serwer. Ograniczenie: serwer wysyła `SwapchainState` proaktywnie po każdym present; klient czeka na lokalny warunek.
**(e)** Pułapka 1: `VK_ERROR_OUT_OF_DATE_KHR` / `VK_SUBOPTIMAL_KHR` — klient musi zwrócić te kody *lokalnie*, na podstawie stanu okna (rozmiar, widoczność), nie czekając na serwer. Naiwne „zawsze VK_SUCCESS” powoduje, że gra renderuje do nieistniejącego obrazu. Pułapka 2: semafor/fence z acquire musi być sygnalizowany przez serwer, ale klient zwraca go aplikacji *natychmiast* — więc klient musi mieć lokalny semafor, który serwer „adoptuje” (mapowanie wire_id ↔ lokalny semafor). Pułapka 3: `vkAcquireNextImageKHR` może zwrócić ten sam indeks, który jest w użyciu, jeśli gra nie zniszczyła poprzedniego — to błąd aplikacji, ale nasz klient musi to wykryć lokalnie (assert), nie na serwerze.

---

## 3.7 `vkQueuePresentKHR`

**(a)** async (fire-and-forget), ale to **punkt końca klatki** — tu domykamy wszystkie dirty pages i wysyłamy `Present`.
**(b)** Cache: stan swapchaina (patrz 3.6). Serwer po `vkQueuePresentKHR` koduje obraz (trik 4) i wysyła wideo.
**(c)** 0 RT. Wynik `VkResult` z present jest spekulatywny (patrz 3.10).
**(d)** Najgorszy: gra prezentuje z `VkPresentInfoKHR` z wieloma semaforami wait — klient musi je wszystkie odtworzyć na serwerze. Ograniczenie: paczka `MemDiff + Cmd[] + Submit + Present` jedna ramka.
**(e)** Pułapka: `vkQueuePresentKHR` zwraca `VK_ERROR_OUT_OF_DATE_KHR` — klient musi to wykryć lokalnie (rozmiar okna), inaczej gra nie odtworzy swapchaina. Druga: `VkPresentInfoKHR::pResults` (dla wielu swapchainów) — każdy wynik osobno. Trzecia: present nie jest barierą pamięci — nie wolno nam na nim czekać z wysyłką dirty pages *po* present, bo to opóźnia klatkę; wysyłamy *przed*.

---

## 3.8 Rejestrowanie bufora poleceń (`vkBeginCommandBuffer` … `vkEndCommandBuffer`, `vkCmd*`)

**(a)** async (fire-and-forget). `vkBeginCommandBuffer`/`vkEndCommandBuffer` to lokalne znaczniki; `vkCmd*` są kodowane do strumienia.
**(b)** Klient koduje `vkCmd*` do bufora `Vec<u8>` per `VkCommandBuffer` (wire_id). `vkEndCommandBuffer` zamyka bufor; `vkQueueSubmit` wysyła go w paczce. Cache: stan `VkCommandBuffer` (recording/executable/pending), stan render pass, stan dynamiczny (viewport, scissor, blend constants).
**(c)** 0 RT. Cały bufor poleceń idzie jedną ramką przy submit.
**(d)** Najgorszy: gra nagrywa bufor poleceń *wielokrotnie* (re-record per klatka) — to normalne, ale oznacza, że każda klatka wysyła pełny bufor (np. 50–200 KB). Ograniczenie: kompresja zstd z kontekstem strumieniowym (trik 1) + dyferencjowanie bufora względem poprzedniej wersji (trik 2) — jeśli gra re-nagrywa ten sam bufor z małymi zmianami, wysyłamy tylko diff. To wymaga, żeby klient trzymał poprzednią wersję bufora.
**(e)** Pułapka 1: `vkCmd*` mogą być wywołane *poza* `vkBeginCommandBuffer`/`vkEndCommandBuffer` (błąd aplikacji) — klient musi to wykryć lokalnie. Pułapka 2: `vkCmdExecuteCommands` (secondary command buffers) — zagnieżdżone bufory; klient musi je rozwinąć lub wysłać referencje (wire_id), a serwer odtworzyć. Naiwne „wyślij wszystko spłaszczone” psuje `vkCmdExecuteCommands` z `VkCommandBufferInheritanceInfo`. Pułapka 3: `vkResetCommandBuffer` — musi wyczyścić lokalny bufor *i* wysłać reset do serwera, inaczej serwer wykona stare polecenia. Pułapka 4: `vkCmdUpdateBuffer` — to zapis do pamięci *na serwerze*, nie przez cieniowanie; klient musi wysłać dane w ramce (nie przez `MemDiff`), bo serwer nie ma tych danych w cieniowanej pamięci.

---

## 3.9 Zestawy deskryptorów (`vkUpdateDescriptorSets`, push descriptors)

**(a)** `vkUpdateDescriptorSets` = async (fire-and-forget); push descriptors = async (część bufora poleceń).
**(b)** Klient trzyma lokalną kopię zawartości `VkDescriptorSet` (mapowanie binding → `VkDescriptorBufferInfo`/`VkDescriptorImageInfo`). `vkUpdateDescriptorSets` aktualizuje kopię i wysyła `DescriptorUpdate{set_id, writes[]}`. Push descriptors są kodowane w `vkCmdPushDescriptorSetKHR` (część bufora poleceń).
**(c)** 0 RT.
**(d)** Najgorszy: gra aktualizuje sety deskryptorów co klatkę (typowe dla DXVK) — wysyłamy pełne `VkWriteDescriptorSet` z uchwytami. Ograniczenie: dyferencjowanie — jeśli binding się nie zmienił, nie wysyłamy. Drugi najgorszy: `vkUpdateDescriptorSets` z `descriptorCopyCount > 0` (`VkCopyDescriptorSet`) — kopiowanie między setami; klient musi to odtworzyć lokalnie i wysłać wynik, nie operację.
**(e)** Pułapka 1: `VkDescriptorBufferInfo::offset`/`range` — jeśli gra używa `VK_WHOLE_SIZE`, klient musi rozwinąć do rozmiaru bufora. Pułapka 2: `VkDescriptorImageInfo::imageLayout` — musi być zgodny z layoutem w `vkCmdPipelineBarrier`; klient nie waliduje (to robi walidacja warstwy), ale serwer musi dostać poprawny layout. Pułapka 3: **push descriptors** nie są częścią `VkDescriptorSet` — nie ma wire_id; muszą iść w buforze poleceń. Naiwne „traktuj jak update seta” psuje kolejność (push descriptor obowiązuje tylko w obrębie bufora).

---

## 3.10 Pipeline cache i tworzenie pipeline (`vkCreateGraphicsPipelines`, `vkCreateComputePipelines`, `vkGetPipelineCacheData`, `vkMergePipelineCaches`)

**(a)** `vkCreate*Pipelines` = OptimisticCreate (async, klient nadaje wire_id); `vkGetPipelineCacheData` = sync (dane z serwera); `vkMergePipelineCaches` = async.
**(b)** Klient nadaje wire_id pipeline'owi natychmiast. Serwer tworzy pipeline asynchronicznie. `vkGetPipelineCacheData` — serwer wysyła dane cache (może być duże, MB) po `vkCreate*Pipelines`; klient cache'uje lokalnie i zwraca z `vkGetPipelineCacheData` bez RT.
**(c)** `vkCreate*Pipelines`: 0 RT (optymistycznie). `vkGetPipelineCacheData`: 0 RT jeśli cache lokalny aktualny; 1 RT przy pierwszym wywołaniu.
**(d)** Najgorszy: kompilacja pipeline'u na serwerze trwa 100–500 ms (shader compilation), a gra od razu go używa w `vkCmdBindPipeline` — serwer musi mieć pipeline gotowy przed wykonaniem. Ograniczenie: **bariera kolejności** — `vkCmdBindPipeline` w buforze poleceń jest wysyłany po `vkCreate*Pipelines` w tej samej ramce; serwer wykonuje sekwencyjnie, więc czeka na kompilację. To blokuje serwer, nie klienta. Drugi najgorszy: `vkGetPipelineCacheData` na dużym cache (10+ MB) — wysyłamy raz, potem tylko diff.
**(e)** Pułapka 1: **błąd kompilacji** — jeśli serwer zwróci `VK_ERROR_INVALID_SHADER_NV` lub podobny, klient już zwrócił `VK_SUCCESS` z wire_id. Rozwiązanie: serwer wysyła `PipelineError{wire_id, result}` asynchronicznie; klient przy następnym użyciu pipeline'u (w `vkCmdBindPipeline`) wykrywa błąd i przechodzi w `VK_ERROR_DEVICE_LOST` (patrz 3.11). To jest świadomy kompromis — lepszy niż RT na każdy pipeline. Pułapka 2: `VkPipelineCache` z `VK_PIPELINE_CACHE_CREATE_EXTERNALLY_SYNCHRONIZED_BIT` — gra synchronizuje sama, my nie możemy. Pułapka 3: `vkMergePipelineCaches` — scalanie po stronie serwera, klient musi zaktualizować lokalny cache.

---

## 3.11 `vkCreate*` / `vkAllocate*` i błędy (device lost)

**(a)** OptimisticCreate (async, klient nadaje wire_id).
**(b)** Klient nadaje wire_id natychmiast i zwraca `VK_SUCCESS`. Serwer tworzy zasób asynchronicznie.
**(c)** 0 RT.
**(d)** Najgorszy: `vkAllocateMemory` na duży rozmiar, który serwer odrzuci (`VK_ERROR_OUT_OF_DEVICE_MEMORY`). Ograniczenie: **wstępna rezerwacja** — przy `vkCreateDevice` klient pyta serwer o `VkPhysicalDeviceMemoryProperties` (migawka, 0 RT) i rezerwuje budżet; `vkAllocateMemory` ponad budżet jest odrzucane lokalnie. Drugi najgorszy: `vkCreateBuffer` z `VK_BUFFER_USAGE_*` nieobsługiwanym przez serwer — klient waliduje lokalnie na podstawie migawki `VkPhysicalDeviceFeatures`/`VkPhysicalDeviceProperties`.
**(e)** Pułapka 1: **device lost** — jeśli serwer zgłosi błąd asynchronicznie, klient musi przejść w `VK_ERROR_DEVICE_LOST` przy następnym wywołaniu, które zwraca `VkResult`. Naiwne „ignoruj błąd” powoduje, że gra używa nieistniejącego zasobu i crashuje w losowym miejscu. Pułapka 2: `vkDestroy*` na zasobie, który serwer odrzucił — klient musi wysłać `Destroy` mimo błędu (serwer ignoruje nieznany wire_id). Pułapka 3: `vkAllocateMemory` z `VkMemoryAllocateInfo::pNext` (`VkMemoryDedicatedAllocateInfo`, `VkMemoryAllocateFlagsInfo`) — generator musi obsłużyć łańcuchy pNext (patrz 3.12).

---

## 3.12 Łańcuchy pNext

**(a)** async (część każdego polecenia).
**(b)** Generator z `vk.xml` generuje kodery dla wszystkich struktur z `sType` i `pNext`. Klient koduje łańcuch rekurencyjnie; serwer dekoduje do lokalnych struktur z `pNext` wskazującym na stos.
**(c)** 0 RT.
**(d)** Najgorszy: łańcuch z 5+ strukturami rozszerzeń (np. `VkImageCreateInfo` + `VkImageDrmFormatModifierExplicitCreateInfoEXT` + `VkExternalMemoryImageCreateInfo` + …). Ograniczenie: generator obsługuje wszystkie struktury z `vk.xml`; nieznane `sType` (z rozszerzeń, których nie znamy) — klient odrzuca lokalnie z `VK_ERROR_EXTENSION_NOT_PRESENT`.
**(e)** Pułapka 1: **`pNext` z pointerami do danych** (np. `VkShaderModuleCreateInfo::pCode`) — trzeba skopiować dane do payloadu, nie wskaźnik. Generator musi znać typy wskaźników z `vk.xml` (`<member><type>uint32_t</type><name>pCode</name><len>codeSize</len></member>`). Pułapka 2: **`sType` musi być zachowany** — serwer odtwarza struktury z poprawnym `sType`, inaczej walidacja Vulkan krzyczy. Pułapka 3: **struktury z `pNext` na wyjściu** (np. `VkPhysicalDeviceProperties2`) — to Query, nie async (patrz 3.16). Pułapka 4: **`VkBaseOutStructure` vs `VkBaseInStructure`** — kierunek łańcucha ma znaczenie; generator musi to rozróżnić.

---

## 3.13 Wysyłanie SPIR-V (`vkCreateShaderModule`)

**(a)** OptimisticCreate (async, klient nadaje wire_id).
**(b)** Klient wysyła `pCode` (SPIR-V) w ramce. Serwer tworzy `VkShaderModule`. Cache: hash SPIR-V → wire_id; jeśli ten sam shader był już wysłany, wysyłamy tylko referencję (trik 2).
**(c)** 0 RT.
**(d)** Najgorszy: duży shader (100+ KB) wysyłany co klatkę (niektóre silniki tworzą shadery dynamicznie). Ograniczenie: cache hash → wire_id; drugi raz wysyłamy tylko 8 B hash. Drugi najgorszy: shader z błędem kompilacji — serwer zwraca błąd asynchronicznie (patrz 3.10).
**(e)** Pułapka 1: `VkShaderModuleCreateInfo::codeSize` musi być wielokrotnością 4 i zgodne z `pCode` — klient waliduje lokalnie. Pułapka 2: **SPIR-V jest nieprzenośny między sterownikami** — jeśli serwer ma inne rozszerzenia niż klient (np. `VK_KHR_shader_float16_int8`), kompilacja może się nie udać. Rozwiązanie: migawka `VkPhysicalDeviceFeatures`/`VkPhysicalDeviceProperties` przy `Hello`; klient waliduje `VkShaderModuleCreateInfo` względem migawki. Pułapka 3: `VkPipelineShaderStageCreateInfo::pName` (entry point) — musi być zachowany; generator koduje napisy (LEB128 + UTF-8).

---

## 3.14 Duże transfery tekstur/buforów (staging)

**(a)** async (fire-and-forget), ale z **dyferencjowaniem** (trik 2).
**(b)** Klient śledzi zawartość buforów/tekstur w pamięci cieniowanej. `vkCmdCopyBuffer`/`vkCmdCopyBufferToImage`/`vkCmdCopyImageToBuffer` są kodowane w buforze poleceń; serwer wykonuje je na swoich zasobach. Jeśli gra pisze do staging buffer przez `vkMapMemory`, dirty pages idą przez `MemDiff` (3.1/3.2).
**(c)** 0 RT.
**(d)** Najgorszy: ładowanie tekstury 4K (64 MB) przez staging buffer — wysyłamy 64 MB przez sieć. Przy 1 Gb/s to ~0,5 s. Ograniczenie: kompresja zstd (tekstury często się kompresują 2–4×) + dyferencjowanie (jeśli tekstura się nie zmieniła, nie wysyłamy). Drugi najgorszy: `vkCmdCopyImage` między teksturami na serwerze — nie wysyłamy danych, tylko polecenie (0 B).
**(e)** Pułapka 1: **staging buffer jest mapowany i pisany przez CPU** — dane idą przez `MemDiff`, nie przez `vkCmdUpdateBuffer`. Naiwne „wyślij `vkCmdUpdateBuffer` z danymi” dubluje dane (raz przez `MemDiff`, raz przez polecenie). Pułapka 2: `vkCmdCopyBufferToImage` z `VkBufferImageCopy::bufferOffset` — offset w buforze staging; serwer musi mieć te dane (z `MemDiff`). Pułapka 3: **format tekstury** — `VK_FORMAT_*` musi być obsługiwany przez serwer; klient waliduje względem migawki `VkPhysicalDeviceImageFormatProperties`.

---

## 3.15 `vkGetQueryPoolResults`

**(a)** sync (wymaga danych z serwera), ale **spekulatywne** — serwer wysyła wyniki proaktywnie po `vkQueueSubmit` z `vkCmdEndQuery`.
**(b)** Serwer po wykonaniu `vkCmdEndQuery` wysyła `QueryResult{pool_id, query, data[]}` asynchronicznie. Klient cache'uje wyniki. `vkGetQueryPoolResults` zwraca z cache, jeśli dostępne; inaczej czeka na RT.
**(c)** 0 RT w stanie ustalonym (wyniki przyszły proaktywnie). 1 RT przy pierwszym wywołaniu po `vkCmdEndQuery`, jeśli wynik jeszcze nie dotarł.
**(d)** Najgorszy: gra odpytuje `VK_QUERY_TYPE_TIMESTAMP` co klatkę dla profilowania — serwer wysyła wyniki proaktywnie, klient ma je gotowe. Drugi najgorszy: `VK_QUERY_TYPE_OCCLUSION` z `VK_QUERY_RESULT_WAIT_BIT` — gra czeka na wynik, który serwer jeszcze liczy. Ograniczenie: `VK_QUERY_RESULT_WAIT_BIT` jest rzadkie w grach (zwykle `VK_QUERY_RESULT_WITH_AVAILABILITY_BIT`); jeśli gra czeka, RT = RTT.
**(e)** Pułapka 1: `VkQueryResultFlags` — `VK_QUERY_RESULT_64_BIT`, `VK_QUERY_RESULT_WITH_AVAILABILITY_BIT`, `VK_QUERY_RESULT_PARTIAL_BIT`. Klient musi zwrócić poprawne flagi i dane; naiwne „zawsze 64-bit” psuje 32-bitowe zapytania. Pułapka 2: **wynik niedostępny** — `VK_QUERY_RESULT_WITH_AVAILABILITY_BIT` zwraca 0 w availability, jeśli wynik jeszcze nie dotarł; klient musi to zwrócić, nie czekać. Pułapka 3: `vkCmdResetQueryPool` — musi wyczyścić cache lokalny *i* wysłać reset do serwera.

---

## 3.16 `vkGetPhysicalDevice*` / `vkEnumerate*` (migawka)

**(a)** spekulatywne (migawka przy