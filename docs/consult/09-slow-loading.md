# Konsultacja 09: wolne ładowanie Dota 2 (DeepSeek na .2)

**Przyczyny (od najprawdopodobniejszej):**

1. **Synchroniczne round-tripy w gorącej ścieżce.** 650× `vkGetImageMemoryRequirements` + 553× `vkCreateGraphicsPipelines` (z `FAIL_ON_PIPELINE_COMPILE_REQUIRED` = pełna kompilacja na serwerze, blokująco) + 870× `vkCreateDescriptorUpdateTemplate` — każde to 1 obieg TCP z serializacją. 0,2 ms RTT × tysiące = minuty, a przy braku pipeliningu (czekanie na odpowiedź przed następnym poleceniem) mnoży się przez narzut Pythona/sterownika po obu stronach.
2. **Brak pipeline cache / rekompilacja.** 553 potoki bez współdzielonego `VkPipelineCache` po stronie serwera → każdy kompiluje shadery od zera (3090 Ti szybkie, ale setki kompilacji × synchronicznie).
3. **Upload tekstur seryjnie w 1600 cyklach Reset/Begin/End/Submit.** Każdy cykl = osobny submit + `vkCmdCopyBufferToImage`; brak batchowania i brak kompresji danych obrazu (LZ4 tylko na powrocie, nie na upload).
4. **Shadow MemWrite w partiach przy każdym Submit** — przy 1600 submitach to 1600 partii małych zapisów, każda z narzutem serializacji i wysyłki; brak koalescencji.

**Tanie pomiary (po jednym):**
1. Timestampy per-typ polecenia po stronie ICD (klient) — histogram czasu oczekiwania na odpowiedź; zobaczysz, czy 90% czasu to Query.
2. Licznik trafień `VkPipelineCache` na serwerze (get/reuse) + czas `vkCreateGraphicsPipelines`.
3. Czas i rozmiar każdej partii MemWrite oraz liczba submitów/s — porównaj z 40–70 poleceń/s.
4. `perf`/`py-spy` na demonie — gdzie 25% CPU (serializacja? GIL?).

**Największy zysk architektoniczny:** **pipelining synchronicznych zapytań + cache po stronie serwera razem**, ale jeśli wybrać jedno: **asynchroniczny, wsadowy pipeline dla Query** — klient wysyła N zapytań naraz (okno), serwer odpowiada batchowo, klient kontynuuje niezależne polecenia. To zamienia tysiące RTT w kilka. Równolegle: **trwały `VkPipelineCache` na serwerze** (klucz: hash SPIR-V + stan) i **cache `vkGetImageMemoryRequirements`** po hashu (format, rozmiar, usage, flags) — 650 zapytań spada do ~kilkudziesięciu unikalnych. Dedup shaderów (2475 modułów → hash SPIR-V) i kompresja uploadów to tańsze dodatki; bez pipeliningu Query i tak nie uratują tempa.
