> **Źródło: DeepSeek V4.1 Flash (lokalnie, `.2`), konsultacja część 3, 2026-11-01. Nieweryfikowane. Pełna odpowiedź (finish: stop).**
> **Odchylenie od rekomendacji §4 (z dowodem zmierzonym, `docs/experiments/shadow-tracking-results.md`):** konsultacja wybiera haszowanie stron i odrzuca `userfaultfd`, bo "wymaga `unprivileged_userfaultfd=1`". To nieprawda dla `UFFD_USER_MODE_ONLY`, a zmierzone `userfaultfd WP_ASYNC + PAGEMAP_SCAN` kosztuje 1,78 µs/stronę (3,4x taniej niż `mprotect`+`SIGSEGV`) i nie ma ryzyka nieaktualnych danych, które niesie częściowe haszowanie (przy `vkQueueSubmit` nie wiemy, które strony odczyta GPU). **Decyzja: domyślnie `WP_ASYNC`+`PAGEMAP_SCAN` (jądro >= 6.7); haszowanie jako rezerwa dla starszych jąder.** Pozostałe zalecenia (§5 NVENC+HEVC, VA-API, wirtualny swapchain; §6 etapy M0–M7) przyjęte jako domyślne.

# Sekcja 4. Pamięć cieniowa

## Porównanie trzech podejść

Kontekst: pamięć cieniowa to lokalny `memfd` mapowany RW przez klienta (gra) i RO/stale przez serwer. Klient musi wykryć zmiany stron i wysłać różnice. Rozważamy trzy mechanizmy detekcji.

### 4.1 `mprotect` + SIGSEGV (write-protect)

- **Koszt na stronę (pierwsze dotknięcie)**: 1× SIGSEGV (~2–5 µs z `sigreturn` na x86-64, w tym przejście jądro/user i `rt_sigreturn`), plus `mprotect(PROT_READ|PROT_WRITE)` na 4 KiB (~1–2 µs, TLB shootdown jeśli mapowanie współdzielone). Razem **~4–8 µs/stronę** przy pierwszym zapisie po flushu.
- **Koszt na stronę (kolejne zapisy w tej samej epoce)**: 0 — strona jest już RW, dopóki nie zrobimy `mprotect(PROT_READ)` przy flushu.
- **Opóźnienie**: dodawane synchronicznie do wątku gry, który pisze. Przy 4 KiB i 8 GB/s to 2 mln stron/s → **8–16 s CPU na sekundę gry** — zabójcze dla dużych wgrywań.
- **Złożoność**: średnia. Handler musi być async-signal-safe: tylko `mprotect` (jest AS-safe), zapis do lock-free kolejki dirty (ring z atomowym `fetch_add` na indeksie), `sigreturn`. **Nie wolno** `malloc`, `write`, `pthread_mutex`. Trzeba użyć `sigaction` z `SA_SIGINFO|SA_RESTART`, `sigaltstack` (SIGSEGV na przepełnionym stosie), i `SA_NODEFER` ostrożnie. Ryzyko: SIGSEGV w handlerze = abort.
- **Jądro 6.x**: działa wszędzie. `userfaultfd` wymaga `vm.unprivileged_userfaultfd=1` lub CAP_SYS_PTRACE — to problem w dystrybucjach.
- **Hugepages**: z THP 2 MiB jedno `mprotect` chroni 2 MiB, ale SIGSEGV przychodzi raz na 2 MiB → **512× mniej sygnałów** na jednostkę danych. To duża zaleta. Wadą: granularność różnic rośnie do 2 MiB (patrz 4.3).
- **HOST_COHERENT**: bez znaczenia dla detekcji; flush i tak robimy przy `vkQueueSubmit`.

### 4.2 `userfaultfd` w trybie write-protect

- **Koszt na stronę**: fault trafia do wątku obsługi przez `read(uffd)` (~1–3 µs round-trip przez kolejkę jądra), potem `UFFDIO_WRITEPROTECT` z `mode=0` na stronę (~1–2 µs). Razem **~3–6 µs/stronę**, ale **asynchronicznie** — wątek gry czeka na `UFFDIO_COPY`/`UFFDIO_WRITEPROTECT` zanim wznowi, więc opóźnienie na wątku piszącym jest podobne do SIGSEGV, tylko przeniesione do dedykowanego wątku obsługi (który i tak musi odpowiedzieć).
- **Złożoność**: wyższa. Osobny wątek, `poll` na uffd, obsługa `UFFD_EVENT_PAGEFAULT` z `flags & UFFD_PAGEFAULT_FLAG_WP`, koordynacja z flush. API jest stabilne od 4.11, WP od 5.7.
- **Jądro 6.x**: `vm.unprivileged_userfaultfd` domyślnie 0 w wielu distro (Debian/Ubuntu/Fedora). Bez roota/capa nie zadziała. **To dyskwalifikuje dla produktu konsumenckiego.**
- **Hugepages**: WP działa na 4 KiB nawet w THP (jądro rozbija huge page przy WP — kosztowne). Efektywnie tracisz THP.
- **Duże wgrywania**: te same problemy co SIGSEGV, plus narzut na wątek.

### 4.3 Haszowanie stron (porównanie z ostatnio wysłaną wersją)

- **Koszt na stronę**: odczyt 4 KiB + hash (np. xxh3 ~1–2 GB/s na rdzeń, czyli ~2–4 µs/4 KiB; albo CRC32C sprzętowy ~10 GB/s → ~0,4 µs/4 KiB). Dla 2 MiB THP: xxh3 ~1 ms/stronę, CRC32C ~0,2 ms. **Brak kosztu na wątku gry** — skanowanie robi wątek tła.
- **Opóźnienie**: brak synchronicznego kosztu dla gry. Ale detekcja jest **okresowa** (skan co N ms), więc różnice są wysyłane z opóźnieniem do N ms. Dla ścieżki gorącej (uniformy, descriptor sets) to za dużo, jeśli N > 1–2 ms.
- **Złożoność**: niska. Tablica `u64 hashes[]` + `u64 sent_hashes[]`, skan przyrostowy (round-robin po regionach), porównanie. Zero sygnałów, zero uffd, zero wymagań jądra.
- **Jądro 6.x**: brak wymagań.
- **Hugepages**: naturalnie — hashujesz 2 MiB, wysyłasz 2 MiB różnicy tylko gdy się zmieniło. Dla dużych wgrywań (tekstury) to **najlepsze** rozwiązanie: jednorazowy zapis 100 MB → jeden skan, jedna wysyłka.
- **HOST_COHERENT**: bez znaczenia.

## Rekomendacja dla ścieżki gorącej

**Hybryda: haszowanie stron jako domyślne + jawny flush jako wymuszony punkt synchronizacji.**

Uzasadnienie liczbowe:
- Ścieżka gorąca w grze AAA przez DXVK to głównie **małe, rzadkie zapisy** do pamięci HOST_COHERENT (uniform buffers, staging) — rzędu 1–50 MB/s, nie 8 GB/s. 8 GB/s to szczyt przy wgrywaniu tekstur/geometrii na starcie poziomu.
- Haszowanie 4 KiB CRC32C: ~0,4 µs/stronę. Dla 256 MiB pamięci cieniowej = 65536 stron × 0,4 µs = **26 ms pełny skan**. Skan przyrostowy 1/16 regionu na klatkę (16,6 ms) = **1,6 ms/klatkę** na wątku tła — akceptowalne.
- Dla ścieżki gorącej (uniformy) skanujemy **tylko regiony oznaczone jako HOST_COHERENT + dynamiczne** co klatkę (kilka MiB) → <0,2 ms.
- `vkFlushMappedMemoryRanges` i `vkQueueSubmit` **wymuszają** natychmiastowy skan wskazanych zakresów (nie czekamy na round-robin) — to gwarantuje poprawność bez opóźnienia.

Dlaczego nie SIGSEGV: przy wgrywaniu 100 MB tekstury mamy 25600 stron × ~6 µs = **154 ms** zablokowanego wątku gry. To widoczny hitch. Haszowanie: 100 MB / 10 GB/s CRC32C = **10 ms** na wątku tła, gra nie czeka.

Dlaczego nie userfaultfd: wymóg `unprivileged_userfaultfd=1` wyklucza produkt.

**Wyjątek**: dla regionów, które gra pisze **rzadko ale precyzyjnie** (np. descriptor sets, command buffers w pamięci HOST_VISIBLE), można włączyć SIGSEGV na **pojedynczych stronach** (nie całych regionach) — koszt 1 sygnału na stronę na epokę jest wtedy pomijalny. To optymalizacja późniejsza (M5+), nie M0.

## Eksperyment do zmierzenia

```
bench_shadow: 256 MiB memfd, 3 tryby (sigsegv, hash_crc32c, hash_xxh3),
wzorce: (a) sekwencyjny zapis 100 MB, (b) losowe 4 KiB co 1 ms przez 5 s,
(c) 1 zapis 64 B co 100 µs przez 5 s.
Metryki: wall time wątku piszącego, CPU time wątku tła, liczba faultów,
liczba stron oznaczonych dirty, opóźnienie detekcji (p50/p99).
Powtórz z THP=madvise i THP=always.
```

Oczekiwane: (a) hash wygrywa 10–15×, (b) sigsegv ~remis z hash (mało stron), (c) hash wygrywa (brak faultów). Jeśli (b) wyjdzie inaczej — zweryfikować koszt `sigreturn` na konkretnym jądrze.

## Kodowanie różnic

**Rozmiar strony**: 4 KiB jako jednostka detekcji (z THP 2 MiB jako jednostką transmisji, gdy cała huge page jest dirty). Nagłówek różnicy: `{region_id: u32, page_index: u32, kind: u8}`.

**Wariant A — XOR + zstd**: dla każdej dirty strony `xor(new, sent)` → bufor, kompresja zstd poziom 1 (szybki, ~500 MB/s). Zaleta: strony częściowo zmienione (np. 64 B w 4 KiB) kompresują się do ~50–100 B. Wada: trzeba trzymać kopię `sent` (podwaja pamięć cieniową) i robić XOR (4 KiB memcpy-class, ~0,3 µs).

**Wariant B — surowe zakresy**: wykryj najdłuższy ciągły zakres zmienionych bajtów w stronie (scan 4 KiB, ~0,5 µs), wyślij tylko ten zakres. Zaleta: brak kopii `sent`, prostsze. Wada: dla rozproszonych zmian w stronie wysyła całą stronę.

**Rekomendacja**: **Wariant B dla stron < 25% zmienionych, Wariant A (XOR+zstd) dla reszty**. W praktyce: uniform buffers (kilka setek B) → B; tekstury → B (całe strony i tak się zmieniają); command buffers → mieszanka. Próg 25% mierzony w eksperymencie.

Format na drucie (stream `DIFF`):
```
u32 region_id | u32 page_index | u8 kind (RAW_RANGE|XOR_ZSTD|FULL) |
u16 offset | u16 len | [payload]
```

## Synchronizacja w obie strony

**Host → klient (`vkInvalidateMappedMemoryRanges`)**: serwer wysyła `INVALIDATE{region_id, ranges[]}` po tym, jak GPU zapisało do pamięci (np. `vkCmdCopyBuffer` do mapowanego bufora, albo compute shader). Klient w ICD: oznacza strony jako „do odświeżenia", przy następnym odczycie przez grę (który i tak przechodzi przez nasz `vkMapMemory` — ale mapa jest bezpośrednia, więc nie widzimy odczytów!) — **problem**: gra czyta bezpośrednio z memfd, nie przez API.

Rozwiązanie: **`vkInvalidateMappedMemoryRanges` jest synchroniczne** — ICD czeka na `INVALIDATE` z serwera (round-trip 0,55 ms + czas GPU) i dopiero wtedy aplikuje różnice do memfd. To jest zgodne ze spec (invalidate jest punktem synchronizacji). Dla `vkQueueSubmit` z odczytem wyniku (np. `vkMapMemory` po fence) — to samo. Opóźnienie: 0,55 ms RTT + ~0,1 ms aplikacji = **~0,7 ms** na invalidate. Akceptowalne, bo invalidate jest rzadkie (raz na klatkę dla wyników compute).

**Wyniki zapytań** (`vkGetQueryPoolResults`): serwer wysyła `QUERY_RESULT{pool_id, first, count, data[]}` **z wyprzedzeniem** (trik 3) — po `vkCmdEndQuery` serwer od razu wysyła wynik, klient cache'uje. `vkGetQueryPoolResults` czyta z cache; jeśli brak — czeka na `QUERY_RESULT` (round-trip). Dla `VK_QUERY_RESULT_WAIT_BIT` czekamy, dla `WITHOUT_WAIT` zwracamy `NOT_READY` jeśli cache pusty.

**Fence/semaphore**: serwer wysyła `SIGNAL{fence_id, value}` po wykonaniu; klient mapuje na lokalny `eventfd`/`futex`. `vkWaitForFences` = `poll` na lokalnym futexie ustawianym przez wątek odbiorczy.

---

# Sekcja 5. Ścieżka powrotna obrazu

## Wirtualny swapchain po stronie klienta

Serwer nie ma okna. Klient ma **dwa** konteksty Vulkan:
1. **ICD (nasz fałszywy sterownik)** — widzi `VkSwapchainKHR` gry, ale to nasz obiekt, nie prawdziwy swapchain.
2. **Prezenter** — osobny, prawdziwy `VkInstance`/`VkDevice` na Intel HD 520 (Mesa ANV), z prawdziwym `VkSwapchainKHR` na X11/Wayland.

Przepływ:
- `vkCreateSwapchainKHR` (gra) → ICD tworzy **wirtualny swapchain**: alokuje N obrazów `VkImage` w pamięci cieniowej (format np. `VK_FORMAT_B8G8R8A8_SRGB`, usage `COLOR_ATTACHMENT|TRANSFER_SRC`), rejestruje je w serwerze (`SWAPCHAIN_CREATE{id, format, extent, count}`). Prezenter tworzy **prawdziwy swapchain** na ANV z tym samym formatem/extent (lub najbliższym wspieranym).
- `vkAcquireNextImageKHR` (gra) → ICD zwraca indeks z lokalnego modelu (round-robin, semafor lokalny). **Nie czeka na serwer** — tempo dyktuje powrót wideo (patrz pacing).
- `vkQueuePresentKHR` (gra) → ICD wysyła `PRESENT{swapchain_id, image_index, fence_id}` do serwera, zwraca natychmiast (lub czeka na `PRESENT_DONE` jeśli gra używa fence).
- Serwer: po `vkQueuePresentKHR` na swoim wirtualnym swapchainie robi `vkCmdCopyImage` z obrazu gry do obrazu enkodera, enkoduje, wysyła `VIDEO{frame_id, pts, data}`.
- Klient (wątek odbiorczy): dekoduje VA-API do `VkImage` zaimportowanego z DMA-BUF (`VK_EXT_external_memory_dma_buf` + `VK_KHR_image_format_list`), potem `vkCmdBlitImage`/`vkCmdCopyImage` do obrazu prezenterowego swapchainu, `vkQueuePresentKHR` (prezenter).

**Import DMA-BUF do Vulkan na Intel HD 520**: VA-API dekoduje do `VASurfaceID`, eksportuje jako DMA-BUF (`vaExportSurfaceHandle`), importuje do Vulkan przez `VkImportMemoryFdInfoKHR` + `VkMemoryDedicatedAllocateInfo`. Wymaga `VK_KHR_external_memory_fd` + `VK_EXT_external_memory_dma_buf` (Mesa wspiera). Format: NV12 → konwersja do BGRA przez shader (compute lub fragment) na ANV. **Uwaga**: HD 520 (Skylake) ma ograniczoną przepustowość — 1366×768 BGRA = 4,2 MB/klatkę, 60 fps = 252 MB/s odczytu + zapisu = ~500 MB/s. HD 520 ma ~25 GB/s, więc OK, ale shader konwersji musi być tani (prosty fragment shader z `texture()` NV12 → `vec4`).

**Alternatywa bez DMA-BUF**: dekodowanie VA-API do CPU (`vaDeriveImage` + `vaMapBuffer`), potem `vkCmdCopyBufferToImage` na ANV. Prostsze, ale +4,2 MB memcpy/klatkę (~0,3 ms) i CPU. **Rekomendacja: DMA-BUF** (zero-copy), fallback CPU dla debugowania.

## Wybór kodowania na serwerze

RTX 3090 Ti (Ampere GA102): **NVENC obsługuje H.264 i HEVC, brak AV1** (AV1 encode dopiero Ada). Vulkan Video encode na Ampere: sterownik NVIDIA wspiera `VK_KHR_video_encode_queue` od ~535, ale dojrzałość niższa niż NVENC.

**Rekomendacja: NVENC przez Video Codec SDK, z interopem do obrazu Vulkan.**

Uzasadnienie:
- NVENC na Ampere: H.264 do 4K60, HEVC do 8K30; opóźnienie enkodera ~1–2 ms (tryb low-latency, `NV_ENC_PRESET_P1` + `NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY`).
- Interop Vulkan→CUDA: `VK_KHR_external_memory` + `VK_KHR_external_semaphore` z `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT` (NVIDIA wspiera `cudaImportExternalMemory`). Obraz Vulkan (linear lub tiled z `VK_IMAGE_TILING_LINEAR` dla uproszczenia) → `CUexternalMemory` → `CUgraphicsResource` → `cuGraphicsMapResources` → NVENC `NV_ENC_INPUT_PTR` (registered resource).
- Vulkan Video encode: mniej kodu (brak CUDA), ale sterownik NVIDIA ma historię bugów w early implementations; NVENC SDK jest dojrzały od dekady. **Wybierz NVENC**, Vulkan Video jako przyszła opcja (M7+).

**Kodek**: **HEVC** dla 1366×768@60 — lepsza jakość/bitrate niż H.264 przy tym samym opóźnieniu na Ampere. H.264 fallback dla kompatybilności dekodera (HD 520 wspiera HEVC decode przez VA-API — Skylake ma HEVC 8-bit decode). **HEVC main, 8-bit, `NV_ENC_HEVC_PROFILE_MAIN`**.

**Tryb**: `NV_ENC_PARAMS_RC_CBR` z `NV_ENC_RC_PARAMS.multiPass = NV_ENC_TWO_PASS_QUARTER_RESOLUTION` (low latency), `NV_ENC_INITIALIZE_PARAMS.enableIntraRefresh = 1` (unika keyframe'ów → brak spike'ów bitrate), `frameIntervalP = 1` (brak B-klatek → minimalne opóźnienie), `enableFillerData = 1` (stały bitrate).

## Dekodowanie VA-API

Intel HD 520 (Skylake GT2): VA-API `VAProfileHEVCMain` decode wspierany (iHD driver). Użyj `libva` (MIT) — dozwolone. Potok:
1. `vaCreateConfig(VAProfileHEVCMain, VAEntrypointVLD, ...)`
2. `vaCreateContext`, `vaCreateSurfaces` (NV12, pula N)
3. `vaBeginPicture`/`vaRenderPicture` (slice data z `VIDEO`)/`vaEndPicture`
4. `vaSyncSurface` → `vaExportSurfaceHandle(VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2)` → DMA-BUF
5. Import do Vulkan (ANV), konwersja NV12→BGRA shaderem, present.

Opóźnienie dekodera: ~1–2 ms na klatkę (Skylake HEVC decode ~real-time 1080p).

## Potok klatek, pacing, bitrate

**Ile w locie**: **3 klatki** w potoku (enkoder → sieć → dekoder → present). Uzasadnienie: RTT 0,55 ms, ale enkoder + dekoder + present to ~4–6 ms; 3 klatki dają bufor na jitter bez nadmiernego opóźnienia. Więcej (4–5) tylko jeśli pomiary pokażą dropy.

**Pacing**: tempo dyktuje **powrót wideo**. Klient utrzymuje kolejkę `VecDeque<DecodedFrame>` z PTS. Prezenter prezentuje klatkę, gdy `now >= pts_next` (clock z `CLOCK_MONOTONIC`, PTS z serwera w tej samej bazie przez offset mierzony przy handshake). Jeśli kolejka pusta — powtarzamy ostatnią klatkę (frame repeat) i wysyłamy `PACING_STARVED` do serwera (serwer zwiększa priorytet enkodera / obniża jakość). Jeśli kolejka > 4 — drop najstarszej (klient nie nadąża) i `PACING_OVERFLOW`.

`vkAcquireNextImageKHR` (gra) blokuje do `pts_next` — to naturalnie ogranicza grę do 60 fps.

**Bitrate przy 1366×768@60**: 1366×768 = 1,05 Mpx. HEVC low-latency, jakość „dobra do gry": **8–12 Mb/s** (≈ 0,13–0,2 bpp). Dla porównania: H.264 potrzebowałby 12–18 Mb/s. **Cel 400 Mb/s jest 30–50× zapasem** — mamy ogromny margines; można podnieść do 20–30 Mb/s dla jakości „bez artefaktów" i nadal <10% celu. **Rekomendacja startowa: 12 Mb/s CBR, adaptacyjnie 8–25 Mb/s** na podstawie `PACING_STARVED`/`OVERFLOW` i feedbacku o utracie pakietów.

**Opóźnienie szacunkowe (end-to-end, present→present)**:
- Serwer: copy image + enqueue NVENC: ~0,5 ms
- NVENC encode (P1, ULL): ~1,5 ms
- Sieć (1 Gb/s, 12 Mb/s = 1,5 MB/klatkę → 12 ms transmisji przy 1 Gb/s! **ale** to nie jest opóźnienie, bo pipeline; opóźnienie kolejki ~1 klatka = 16 ms)
- Dekoder VA-API: ~1,5 ms
- Konwersja + present: ~1 ms
- **Razem: ~20–25 ms** (z pipeline 3 klatek). Bez pipeline: ~5 ms + czas transmisji.

**Uwaga**: 12 Mb/s na 1 Gb/s to 1,2% — transmisja klatki 1,5 MB zajmuje 12 ms, ale przy pipeline 3 klatek nie blokuje. Jeśli bitrate wzrośnie do 100 Mb/s, transmisja 12,5 MB = 100 ms — **to zabije pipeline**. Dlatego bitrate musi być < ~50 Mb/s przy 1 Gb/s, żeby transmisja < 50 ms. **Twardy limit: 40 Mb/s** (transmisja 5 MB = 40 ms, pipeline 3 klatek = 120 ms bufora).

---

# Sekcja 6. Plan spięcia M0..M7

Zasada: każdy etap ma **produkt** (działający artefakt), **test bez GPU serwera** (pętla lokalna klient↔serwer na jednej maszynie, lavapipe/llvmpipe jako „serwer"), **metryki**.

Wspólna infrastruktura testowa: `gpu-tools loopback` — uruchamia `gpu-renderd` i `gpu-client` na jednej maszynie, transport przez `AF_UNIX` (zamiast TCP), serwer na lavapipe (`VK_ICD_FILENAMES=lvp_icd.x86_64.json`) lub llvmpipe. Pozwala testować protokół bez sieci i bez NVIDIA.

### M0 — Szkielet protokołu i transportu
- **Produkt**: `gpu-wire` (ramka 16 B, CRC32C, streamy), `gpu-proto` (typy wiadomości, serde), `gpu-tools loopback`. Test: wysyłaj 1M ramek, zmierz throughput i CRC.
- **Zależności**: brak (tylko `crc32c`, `serde` — MIT/Apache).
- **Test bez GPU**: `cargo test` + benchmark `wire_bench` (ramki/s, błędy CRC przy wstrzykiwaniu).
- **Metryki**: `wire.frames_tx`, `wire.frames_rx`, `wire.crc_errors`, `wire.bytes_tx`, `wire.rtt_us`.

### M1 — ICD szkielet + enumeracja
- **Produkt**: `gpu-icd` (cdylib, `vk_icdGetInstanceProcAddr`), `gpu-shadow` (mapa uchwytów wire-id), `gpu-renderd` (lavapipe backend). Gra (`vkcube`) startuje, widzi „GPU", `vkCreateInstance`/`vkEnumeratePhysicalDevices` działają przez sieć.
- **Zależności**: M0.
- **Test bez GPU**: `vkcube` na loopback z lavapipe; `vulkaninfo` przez ICD.
- **Metryki**: `icd.calls_total` (per funkcja), `icd.calls_roundtrip` (ile czekało na serwer), `icd.latency_p50/p99_us`.

### M2 — Zasoby: pamięć, bufory, obrazy
- **Produkt**: `vkAllocateMemory`/`vkBindBufferMemory`/`vkCreateImage` przez sieć; pamięć cieniowa (memfd) + haszowanie CRC32C; `vkMapMemory` zwraca adres memfd; `vkFlushMappedMemoryRanges` wysyła różnice.
- **Zależności**: M1.
- **Test bez GPU**: `vkcube` renderuje (lavapipe), ale obraz nie wraca — sprawdzamy tylko, że nie ma błędów walidacji i że różnice są wysyłane. Test `shadow_bench` (sekcja 4).
- **Metryki**: `shadow.pages_dirty`, `shadow.bytes_sent`, `shadow.scan_us`, `shadow.flush_us`, `shadow.hash_hits` (strony niezmienione).

### M3 — Polecenia i synchronizacja
- **Produkt**: `vkCreateCommandPool`/`vkBeginCommandBuffer`/`vkCmd*`/`vkQueueSubmit`/fence/semaphore/event przez sieć; grupowanie poleceń (trik 1) — buforowanie `vkCmd*` do `vkEndCommandBuffer`, kompresja zstd, wysyłka bez czekania; `vkWaitForFences` na lokalnym futexie.
- **Zależności**: M2.
- **Test bez GPU**: `vkcube` na loopback renderuje do offscreen (lavapipe), fence działają; test `cmd_bench` (polecenia/s, rozmiar po kompresji).
- **Metryki**: `cmd.batches`, `cmd.bytes_raw`, `cmd.bytes_compressed`, `cmd.compress_ratio`, `sync.fence_wait_us`, `sync.submit_us`.

### M4 — Swapchain wirtualny + powrót wideo (loopback)
- **Produkt**: `gpu-video` (NVENC stub → na loopback: enkoder software'owy `x264`/`kvazaar`? **nie** — zależności MIT/Apache: użyj `rav1e`? nie, to AV1. Dla loopback: **`openh264` (BSD) lub `x265` (GPL — odpada)**. Wybór: **`kvazaar` (BSD) dla HEVC** lub `openh264` (BSD) dla H.264). Klient: VA-API stub → na loopback: dekoder software'owy (`openh264`/`kvazaar` decode? kvazaar to tylko encode). **Rekomendacja: H.264 + `openh264` (BSD) encode+decode na loopback**, HEVC/NVENC/VA-API dopiero na sprzęcie. Prezenter: prawdziwy swapchain na ANV/llvmpipe.
- **Zależności**: M3.
- **Test bez GPU**: `vkcube` na loopback: klatka renderowana na lavapipe → enkodowana openh264 → dekodowana openh264 → prezentowana na X11 przez ANV/llvmpipe. **To jest pierwszy end-to-end obraz.**
- **Metryki**: `video.encode_us`, `video.decode_us`, `video.bitrate_mbps`, `video.frames_in_flight`, `video.pacing_starved`, `video.pacing_overflow`, `video.e2e_latency_ms`.

### M5 — Ścieżka gorąca: DXVK/Proton
- **Produkt**: obsługa `vkCmd*` używanych przez DXVK (descriptor sets, pipeline cache, `vkCmdDrawIndexed`, `vkCmdCopyBuffer`), `vkGetQueryPoolResults` z cache, `vkInvalidateMappedMemoryRanges` synchroniczne. Test: prosta gra DXVK (np. `vkd3d-proton` demo lub `dxvk` + `wine` + prosty benchmark).
- **Zależności**: M4.
- **Test bez GPU**: DXVK na loopback z lavapipe (wolne, ale poprawnościowe); mierzymy tylko liczbę wywołań i round-tripów.
- **Metryki**: `icd.calls_roundtrip` (cel: <5% wywołań), `icd.latency_p99_us`, `query.cache_hits`, `invalidate.wait_us`.

### M6 — Sprzęt: NVIDIA + NVENC + VA-API
- **Produkt**: `gpu-renderd` na NVIDIA (prawdziwy Vulkan), `gpu-video` z NVENC (Video Codec SDK, interop CUDA), klient z VA-API (iHD) + DMA-BUF import. Test: `vkcube` i gra AAA przez Proton na prawdziwym sprzęcie.
- **Zależności**: M5.
- **Test bez GPU serwera**: **nie da się** — to etap sprzętowy. Ale: testy regresji na loopback (M4/M5) muszą dalej przechodzić. Testy NVENC: `gpu-tools nvenc_bench` (encode 1366×768@60, zmierz opóźnienie i bitrate). Testy VA-API: `gpu-tools vaapi_bench` (decode HEVC, zmierz opóźnienie).
- **Metryki**: `nvenc.encode_us`, `nvenc.bitrate_mbps`, `vaapi.decode_us`, `video.e2e_latency_ms` (cel: <25 ms), `video.drop_rate`.

### M7 — Optymalizacja i pomiar celu 400 Mb/s
- **Produkt**: adaptacyjny bitrate, tuning kompresji poleceń, optymalizacja ścieżki gorącej (SIGSEGV na wybranych stronach jeśli pomiary uzasadnią), profilowanie.
- **Zależności**: M6.
- **Test bez GPU**: benchmarki na loopback (regresja) + pomiary na sprzęcie.
- **Metryki końcowe (cel)**:
  - `net.total_mbps` — **cel ≤ 400 Mb/s** (suma: polecenia + różnice + wideo + odpowiedzi)
  - `net.cmd_mbps`, `net.diff_mbps`, `net.video_mbps`, `net.resp_mbps` (rozbicie)
  - `video.e2e_latency_ms` — cel < 25 ms
  - `video.fps` — cel 60 (stabilne)
  - `icd.calls_roundtrip_pct` — cel < 5%
  - `shadow.bytes_sent_per_frame` — cel < 1 MB/klatkę
  - `cmd.compress_ratio` — cel > 5×
  - `video.pacing_starved` / `video.pacing_overflow` — cel < 1% klatek

**Uwaga o zależnościach**: `openh264` (BSD-2), `kvazaar` (BSD-3), `libva` (MIT), `zstd` (BSD/GPL dual — wybierz BSD), `crc32c` (Apache/MIT), `serde` (MIT/Apache), `tokio` (MIT). **Video Codec SDK (NVIDIA) jest proprietary** — to nie jest „zależność" w sensie linkowania (używamy przez `dlopen` + własne bindings), ale licencja wymaga akceptacji; dla produktu komercyjnego OK, dla open-source trzeba udokumentować. **Alternatywa**: Vulkan Video encode (bez proprietary SDK) — ale mniej dojrzałe; rozważ jako M7+.

**Ryzyko M6**: interop Vulkan→CUDA na Ampere wymaga `VK_KHR_external_memory_fd` + `cudaImportExternalMemory`; jeśli sterownik NVIDIA ma bug (znane przypadki z tiled images), fallback: `vkCmdCopyImageToBuffer` (linear) → `cuMemcpyHtoD` → NVENC. +1,5 ms/klatkę, ale działa.