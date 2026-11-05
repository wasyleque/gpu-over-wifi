# Analiza wąskiego gardła synchronizacji w Rise of the Tomb Raider (DX11/DXVK)

## Dane bazowe z pomiarów (`icd-rottr-720p.log` + `server-sample.txt`)

| Przebieg | Klatki | Obiegi (`roundtrips`) | Czekanie na serwer (`t_syncrecv`) | `WaitSemaphores` | `GetDescriptorEXT` (serwer) | `GetSemaphoreCounterValue` | Odświeżenia pamięci (`t_refresh`) | Zapisy pamięci (surowo / łącze) | Skan + wysyłka różnic (`t_scan` + `t_memsend`) | Blokada `InstState` (czekanie / trzymanie / wejścia) |
|---|---:|---:|---:|---|---|---|---|---|---|---|
| **720p** (`icd-rottr-720p.log:1926-1931`) | 3 429 | 32 180 (9,4/kl.) | **110 612 ms** (32,3 ms/kl.) | 16 901× = **86 769 ms** (**5 134 µs**) | 5 478× = **18 243 ms** (**3 330 µs**) | 111× = **2 267 ms** (**20 427 µs**) | 7 831× (`11 188 ms`; całe 20 567, zakresy 33 005 po 4 105 KB) | 14,90 GB / 5,76 GB | 7 047 ms + **42 446 ms** = **49 493 ms** | **89 611 ms** / **57 008 ms** / 259 900 (75,8/kl.) |
| **1080p** (`client-sample.txt:1-6`) | 2 429 | 28 805 (11,9/kl.) | **82 650 ms** (34,0 ms/kl.) | 14 785× = **63 897 ms** (**4 322 µs**) | 4 884× = **13 327 ms** (**2 729 µs**) | 119× = **2 194 ms** (**18 435 µs**) | 6 411× (`9 749 ms`; całe 6 491, zakresy 32 058 po 4 099 KB) | 12,25 GB / 5,20 GB | 6 059 ms + **37 358 ms** = **43 417 ms** | **76 280 ms** / **48 681 ms** / 208 461 (85,8/kl.) |
| **Serwer łącznie** (`server-sample.txt:1-2`, 5 845 kl.) | 5 845 | 50 404 `query` | `query`: **1 835 ms** (**36,4 µs**/zap.) | `WaitForFences`: 5 845× = 4 468 ms (764 µs); `WaitSemaphores` poza top-10 (<447 ms, czyli **<26 µs**/wyw.) | mieści się w `query` (1 835 ms łącznie) | mieści się w `query` (1 835 ms łącznie) | `memreaddelta`: **91 872× = 57 548 ms** (**626 µs**/żądanie) | `memwrite`: 1 919 640× = 10 759 ms | `cmds`: 54 962× = 30 785 ms (w tym dekodowanie 12 560 ms) | 1 wątek na połączenie (`server.rs:17-24`, `conn.rs:105-146`) |

---

## 1. Skąd bierze się 5,13 ms (`86 769 ms / 16 901`) na jedno `vkWaitSemaphores`?

Stoper przypisujący czas do polecenia w `rt_by_cmd` (`obiegi wg poleceń`) startuje w [`crates/gpu-icd/src/fwd.rs:1584`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1584) (na końcu `exec_begin`) i zatrzymuje się dopiero w [`crates/gpu-icd/src/fwd.rs:1676-1679`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1676-L1679) (wewnątrz `exec_finish`). Obejmuje więc **nie tylko wykonanie `vkWaitSemaphores`**, ale cały ogon operacji sklejonych z tym wywołaniem w strumieniu i po powrocie z gniazda:

1. **Doczepienie żądań `MemReadDelta` / `MemReadDeltaRanges` do tego samego obiegu (`fwd.rs:1556, 1567-1570` i `1632`):**
   - `vkWaitSemaphores` ma klasę `CmdClass::Sync`. Gdy `st.gpu_dirty == true` (po `vkQueueSubmit`), `exec_begin` doczepia za `Request::Query` i `Request::Sync` żądania `delta_request(id)` dla wszystkich pamięci z `refresh_ids()` ([`fwd.rs:569-575`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L569-L575)), a `Pending::receive()` ([`fwd.rs:1624-1634`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1624-L1634)) **nie kończy się po odebraniu odpowiedzi na `WaitSemaphores`**, lecz czeka na odbiór wszystkich odpowiedzi `deltas` (`recv_at(base + 2 + i)`).
   - Dlaczego `refresh_ids()` zwraca aż ~7 pamięci na każde odświeżenie (`20 567` całych + `33 005` zakresowych = **53 572 żądania `MemReadDelta*` w 7 831 odświeżeniach** w przebiegu 720p), skoro włączono `lazymap`?
     - W [`fwd.rs:1468`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1468) pamięć trafia do `mem_refresh`, gdy `size <= REFRESH_ALWAYS_BELOW (16 MB) || cached`.
     - Natomiast w [`fwd.rs:1853`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1853) `lazymap` wymaga `(cached || lazy_all())`.
     - W efekcie **wszystkie pamięci nie-`HOST_CACHED` (flagi `0x7`, write-combined/BAR) o rozmiarze ≤ 16 MB** (`0x100301` 16 MB `Whole`, `0x100939` 16 MB, `0x10004a` 4 MB, `0x100046` 8 MB itd. — patrz `icd-rottr-720p.log:1350-1724`) **nie dostają `lazymap`, ale są chętne odświeżane z serwera przy każdym `WaitSemaphores`!** Co więcej, to są bufory wysyłkowe CPU->GPU (do `0x100301` klient zapisał 5,38 GB, do `0x10004a` 3,32 GB, do `0x100939` 2,63 GB), które mają flagi zapisu GPU tylko dlatego, że DXVK tworzy bufory D3D11 z uniwersalnym `usage = 0x219ff`.
2. **Wypchnięcie zaległych `MemWrite*` i `Cmds` przed `Query` (`fwd.rs:1550-1564`):**
   - Na wejściu w ścieżkę synchroniczną `exec_begin` woła `if st.scan_pending { st.settle_memory(); }` ([`fwd.rs:1550-1552`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1550-L1552)), co wrzuca do kolejki nadawczej `Out` wszystkie `MemWrite`/`MemWriteLz4` (5,76 GB w 720p), a następnie w `send_many` ([`fwd.rs:1559-1565`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1559-L1565)) dokłada `Request::Cmds` (535 MB w 29 820 partiach) **przed** `Request::Query` (`vkWaitSemaphores`). Jednowątkowy serwer ([`conn.rs:105-146`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-renderd/src/conn.rs#L105-L146)) musi najpierw odebrać z TCP, rozpakować i wykonać te `MemWrite*` oraz `Cmds`, zanim w ogóle dotknie `Query`.
3. **Kolejka do blokady klienta (`WAIT_LOCK_FINISH`) i aplikacja delty (`t_refresh`) wliczane do `p.t_recv` (`fwd.rs:993-1001`, `1662-1679`):**
   - Po odebraniu ramek w `p.receive()` wątek musi ponownie zdobyć `lock_state(inst)` ([`fwd.rs:996`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L996)), po czym w `exec_finish` kopiuje delty i wykonuje dodatkowe obiegi `drain_delta` ([`fwd.rs:1662-1675`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1662-L1675), co zajęło **11 188 ms**), a dopiero w linii **1679** dopisuje `p.t_recv.elapsed()` do `rt_by_cmd["vkWaitSemaphores"]`!

### Podział czasu 5,13 ms (`86 769 ms` w 16 901 obiegach `vkWaitSemaphores`, przebieg 720p = 3 429 klatek)

*(Czasy serwera z `server-sample.txt` [5 845 klatek = przebieg 720p + 1080p] przeskalowane proporcjonalnie liczbą operacji w przebiegu 720p: 53 572 z 91 872 `memreaddelta` [58,3%], 29 820 z 54 962 `cmds` [54,3%], 5,76 GB z 10,96 GB `memwrite` [52,6%]).*

| Składnik czasu jednego obiegu `vkWaitSemaphores` | Czas łączny w 720p (s) | Średnio na 1 `WaitSemaphores` (ms) | Średnio na klatkę (ms/kl.) | Udział | Źródło w logach i kodzie |
|---|---:|---:|---:|---:|---|
| **1. Kolejka na serwerze: obsługa doczepionych `MemReadDelta*`** (53 572 żądań × 0,626 ms: kopia GPU/BAR + haszowanie w `replay.rs:480-518`) | **33,55 s** | **1,99 ms** | 9,78 ms | **38,7%** | `server-sample.txt:1` (`memreaddelta 91872x 57548ms`), `fwd.rs:1567-1570, 1632` |
| **2. Kolejka na serwerze: wykonanie poprzedzających `Cmds` i `MemWrite*`** (29 820 partii `cmds` × 0,56 ms + `memwrite` przed `Query`) | **22,36 s** | **1,32 ms** | 6,52 ms | **25,8%** | `server-sample.txt:1` (`cmds 54962x 30785ms`, `memwrite 10759ms`), `fwd.rs:1551, 1563` |
| **3. Odczyt i aplikacja odpowiedzi odświeżeń na kliencie (`t_refresh`)** (`apply_delta` + `drain_delta` wykonywane przed odczytem stopera `p.t_recv`!) | **11,19 s** | **0,66 ms** | 3,26 ms | **12,9%** | `icd-rottr-720p.log:1926` (`odświeżenia pamięci 7831 (11188 ms)`), `fwd.rs:1662-1679` |
| **4. Sieć (1 Gb/s) + kolejka nadawcza `Out` + kolejka biletów `recv_at`** (wypchnięcie 6,3 GB danych przez `gpu-icd-send` przed ramkami `Query` + RTT 0,37 ms) | **11,92 s** | **0,71 ms** | 3,48 ms | **13,7%** | `icd-rottr-720p.log:1926` (5,76 GB `memw_wire` + 0,54 GB `cmd_bytes`), `link.rs:44-66, 139-164` |
| **5. Kolejka do blokady klienta przy powrocie (`WAIT_LOCK_FINISH`)** (czekanie w `fwd.rs:996` na wątek trzymający `InstState` w `write_mapped`/`send_mem`) | **6,50 s** *(+ ~5,8 s na wejściu przed `t_recv`)* | **0,38 ms** *(+ 0,35 ms przed `t_recv`)* | 1,90 ms | **7,5%** | `icd-rottr-720p.log:1930` (`oczekiwanie 89611 ms w 259900 wejściach` = 0,345 ms/wejście), `fwd.rs:927, 996` |
| **6. Samo wykonanie `vkWaitSemaphores` + `Sync` na serwerze** (`query` średnio 36,4 µs + krótkie oczekiwania GPU) | **~1,25 s** | **~0,07 ms** | 0,36 ms | **1,4%** | `server-sample.txt:1-2` (`query 50404x 1835ms`, `sync 0ms`, brak `WaitSemaphores` w top-10) |
| **RAZEM (`rt_by_cmd["vkWaitSemaphores"]`)** | **86,77 s** | **5,13 ms** | **25,30 ms** | **100%** | `icd-rottr-720p.log:1927` (`WaitSemaphores 16901x 86769ms (5134µs)`) |

> **Wniosek dla `vkGetSemaphoreCounterValue` (20,4 ms) i `vkGetDescriptorEXT` (3,33 ms):** oba polecenia cierpią na dokładnie to samo zjawisko Head-of-Line Blocking — w [`fwd.rs:1550`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1550) każde z nich wywołuje `settle_memory()` (wpychając przed siebie megabajty `MemWriteLz4` i `Cmds`), a w `Shared::recv_at` ([`link.rs:46-52`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/link.rs#L46-L52)) ich bilet czeka w kolejce FIFO za trwającym pociągiem `Cmds + WaitSemaphores + 7× MemReadDelta`.

---

## 2. Ranking 5 zmian wg (zysk / ryzyko / koszt)

| # | Zmiana | Szacowany zysk (720p) | Ryzyko | Koszt |
|---:|---|---|---|---|
| **1** | **Objęcie `lazymap` wszystkich pamięci odświeżalnych (lub wyłączenie odświeżania nie-`HOST_CACHED` ≤ 16 MB)** | **~40–44 s** (~12–13 ms/kl.: znika 33,6 s `memreaddelta` na serwerze i 11,2 s `t_refresh` na kliencie) | Niskie | Bardzo mały (~10 linii w `fwd.rs`) |
| **2** | **Wyjęcie kompresji LZ4 i kodowania ramek `MemWrite*` spod blokady `InstState` do wątku `gpu-icd-send`** | **~38–42 s** czasu trzymania `InstState` (spadek z 57 s do ~15 s) i **~60–70 s** mniej czekania wątków gry na blokadę | Zerowe | Mały (~40 linii w `fwd.rs` i `link.rs`) |
| **3** | **Osobne połączenie „szybkich zapytań” (`FastQuery`) z drugim wątkiem na serwerze dla `WaitSemaphores` / `GetSemaphoreCounterValue` / `WaitForFences` / `GetDescriptorEXT`** | **~25–30 s** (~8 ms/kl.: zapytania omijają kolejkę za `Cmds` i `MemWrite*`, czas obiegu spada z 3–20 ms do ~0,25 ms) | Średnie | Średni (~150 linii) |
| **4** | **Wyeliminowanie 90% chybień `vkGetDescriptorEXT`: rozszerzenie `descsynth` o `(ty, format)` dla `UNIFORM_TEXEL_BUFFER` + pre-cache `SAMPLED_IMAGE`** | **~16–18 s** (~5 ms/kl.: znika 2 652 chybień typu 4 i 2 252 chybień typu 2) | Niskie | Mały (~60 linii w `descsynth.rs` i `fwd.rs`) |
| **5** | **Wejścia bez blokady `InstState` dla `vkBeginCommandBuffer` / `vkResetCommandBuffer` oraz cache `GetDescriptorSetLayout*` i `GetImageMemoryRequirements2`** | **~8–12 s** czekania na blokadę + **~3 s** obiegów (~80 tys. mniej wejść w `lock_state`) | Niskie | Mały (~50 linii w `fwd.rs`) |

### Szczegóły techniczne, ograniczenia Vulkana i warunki poprawności

#### Zmiana 1: Spójność `mem_refresh` i `lazymap` (`fwd.rs:1173, 1468, 1852-1854`)
- **Mechanizm:** Obecnie pamięć `0x10165d` (4 MB, `HOST_CACHED`) z włączonym `lazymap` wygenerowała w 3 429 klatkach zaledwie **558 błędów stron (35,7 MB)** na 3 300 unieważnień (`icd-rottr-720p.log:1929`). Tymczasem 9 pozostałych pamięci (w tym dwa 16 MB bufory pierścieniowe `0x100301` i `0x100939` bez `HOST_CACHED`) nie dostało `lazymap` przez warunek `cached || lazy_all()` w [`fwd.rs:1853`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1853), ale z powodu `size <= REFRESH_ALWAYS_BELOW (16 MB)` w [`fwd.rs:1468`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1468) wymusiło **53 572 chętnych odczytów `MemReadDelta*`** (44,8 s łącznie!). Należy:
  1. Włączyć `lazymap` dla każdej pamięci z `mem_refresh == true` (albo obniżyć `REFRESH_ALWAYS_BELOW` dla pamięci bez `HOST_CACHED` do np. 64 KB — w D3D11/DXVK odczyty CPU z GPU idą przez bufory `D3D11_USAGE_STAGING`, które zawsze mają `HOST_CACHED`).
  2. Jeśli w *Black Book* `lazymap` na buforze bez `HOST_CACHED` zawieszał grę (komentarz w [`fwd.rs:1851`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1851)), przyczyną było nadpisywanie w `lazy_fetch` niezapisanych jeszcze stron CPU lub brak `write_mapped` przed `MemRead` — co jest już naprawione w [`fwd.rs:515-517`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L515-L517) (`self.settle_memory()`).
- **Warunek poprawności:** Dla pamięci objętej `lazymap` po każdym `Sync` wywoływane jest `lazy_invalidate()` ([`fwd.rs:1771-1773`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1771-L1773)), więc każdy odczyt aplikacji ze strony potencjalnie zapisanej przez GPU wyzwoli `userfaultfd` i pobierze z serwera dokładne bajty przez `MemRead`.

#### Zmiana 2: Wyjęcie kompresji LZ4 (`t_memsend_ns = 42,4 s`) spod blokady `InstState` (`fwd.rs:386-418`, `link.rs:139-164`)
- **Mechanizm:** W [`fwd.rs:386-418`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L386-L418) (`send_mem`) wątek trzymający globalny `Mutex<InstState>` sam tnie 14,9 GB danych na kawałki, liczy dla każdego `compress_lz4(chunk)`, koduje `Request::MemWriteLz4` i liczy CRC32 w `Frame::new` ([`link.rs:189`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/link.rs#L189)). To zajmuje **42 446 ms z 57 008 ms** całego czasu trzymania blokady instancji!
  - Pod blokadą `InstState` w `write_mapped` należy jedynie skopiować brudne zakresy z `Shadow` do bufora pośredniego (lub przekazać wycinki z kopii) i wrzucić zadanie ` RawMemWrite { memory, offset, raw_bytes }` do kolejki `Out` w `Link`.
  - Samą kompresję LZ4 (`compress_lz4`) oraz składanie nagłówka `Frame` i CRC32 wykonuje wątek nadawczy `gpu-icd-send` ([`link.rs:139`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/link.rs#L139)).
- **Warunek poprawności:** Kopia bajtów brudnych stron powstaje pod `InstState` w tym samym punkcie czasu (przed `vkQueueSubmit`), a kolejność zadań w kolejce `Out` jest ściśle FIFO, więc serwer otrzyma identyczny strumień bajtów w identycznej kolejności.

#### Zmiana 3: Drugi wątek wykonawczy / szybki kanał zapytań na serwerze a zasady wielowątkowości Vulkana
- **Ograniczenia specyfikacji Vulkan (*Externally Synchronized Parameters*):**
  - Na jednym `VkDevice` funkcje: `vkWaitSemaphores`, `vkGetSemaphoreCounterValue`, `vkWaitForFences`, `vkGetFenceStatus`, `vkGetDescriptorEXT`, `vkGet*MemoryRequirements[2]`, `vkGetDescriptorSetLayout*`, `vkGetBufferDeviceAddress` **są w pełni wielowątkowe (thread-safe)** — żaden ich parametr nie wymaga zewnętrznej synchronizacji względem `VkDevice`, `VkQueue` ani `VkCommandBuffer`.
  - Zewnętrznej synchronizacji na serwerze wymagają jedynie:
    1. `vkQueueSubmit` / `vkQueueWaitIdle` / `vkDeviceWaitIdle` (na `VkQueue`),
    2. `vkResetFences` oraz podanie `VkFence` do `vkQueueSubmit` (na samym uchwycie `VkFence`),
    3. `vkMapMemory` / `vkUnmapMemory` (na `VkDeviceMemory`) oraz mutacje rejestru uchwytów `Registry` (`vkCreate*` / `vkDestroy*` / `vkAllocate*` / `vkFree*`).
- **Mechanizm (osobne połączenie `FastQuery` podłączone do tej samej `Session` na serwerze):**
  - `Registry` na serwerze jest już chroniony lub może używać `RwLock` (odczyt uchwytów w `wire_to_handle` bierze `read()`, rejestracja/wyrejestrowanie w głównym wątku bierze `write()`).
  - Główny wątek połączenia (`STREAM_CONTROL`) obsługuje jak dotąd `MemWrite*`, `Cmds`, `MemRead*` oraz mutacje obiektów i po każdej wykonanej partii `Cmds` o numerze `batch_seq` aktualizuje atomowy licznik `done_batch_seq: AtomicU64` + budzi `Condvar`.
  - Drugi wątek serwera (na drugim gnieździe podłączonym przez `AttachFast { session }` analogicznie do `AttachVideo` w [`conn.rs:135`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-renderd/src/conn.rs#L135)) obsługuje wyłącznie krótkie zapytania tylko-do-odczytu (`vkWaitSemaphores`, `vkGetSemaphoreCounterValue`, `vkWaitForFences`, `vkGetDescriptorEXT`, `vkGet*MemoryRequirements*`).
- **Warunek poprawności (przyczynowość względem `Cmds`):**
  - Każde szybkie zapytanie niesie `min_batch_seq: u64` — numer ostatniej partii `Cmds` wysłanej przez klienta przed tym zapytaniem (dla `vkGetDescriptorEXT` lub `vkWaitSemaphores` czekającego na uchwyt/sygnał utworzony w `st.batch`).
  - Jeśli `st.batch` na kliencie zawiera niewysłane polecenia w chwili wywołania `vkWaitSemaphores`/`vkGetSemaphoreCounterValue`, klient najpierw wypycha `st.batch` na główny strumień i przekazuje jego `batch_seq` w szybkim zapytaniu.
  - Wątek `FastQuery` na serwerze czeka na `done_batch_seq >= min_batch_seq` tylko wtedy, gdy `min_batch_seq` jeszcze nie dotarło, ale **nie czeka na `MemReadDelta` ani na kolejne `MemWrite*`/`Cmds` wysłane przez inne wątki po nim!**

#### Zmiana 4: Likwidacja chybień `vkGetDescriptorEXT` dla `UNIFORM_TEXEL_BUFFER` (2 652) i `SAMPLED_IMAGE` (2 252)
- **Mechanizm:**
  1. W [`icd-rottr-720p.log:1926`](file:///home/wasyl/.claude/jobs/05a9919e/tmp/an/icd-rottr-720p.log) tablica `desc_miss_by_type` to `[23, 0, 2252, 62, 2652, 32, 420, 37, ...]`:
     - Indeks `4` (`VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER` = **2 652 chybienia**, 48% wszystkich) oraz indeks `5` (`STORAGE_TEXEL_BUFFER` = 32) przyjmują w `VkDescriptorGetInfoEXT` ten sam `VkDescriptorAddressInfoEXT { sType, pNext, address, range, format }` co bufory `UNIFORM_BUFFER`! Obecnie [`descsynth::input_of`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L735) obsługuje tylko typy `6` i `7` (gdzie `format == 0`). Rozszerzenie klucza w `descsynth::Synth` z `ty: u32` na `(ty: u32, format: u32)` pozwoli wyuczyć wzorzec bitowy osobno dla każdego używanego `VkFormat` (w grze jest ich <10) i liczyć `UNIFORM_TEXEL_BUFFER` lokalnie w `descriptor_lockfree` bez blokady i bez sieci.
     - Indeks `2` (`VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE` = **2 252 chybienia**, 41% wszystkich): przy `vkCreateImageView` serwer może od razu w `Replayer::run` wyliczyć deskryptor `SAMPLED_IMAGE` dla standardowego layoutu `VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL` (oraz `GENERAL`) i dopisać go do kolejki zwrotnej odbieranej przy `Sync`.
- **Warunek poprawności:** Identyczny rygor jak w obecnym `descsynth` ([`fwd.rs:1722`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1722)): lokalna synteza dla pary `(ty, format)` włącza się dopiero po potwierdzeniu wzorca na `LEARN_NEEDED` (32) odpowiedziach serwera i wyłącza natychmiast przy jakiejkolwiek niezgodności bitowej.

#### Zmiana 5: Odchudzenie wejść w `lock_state` (`fwd.rs:917-960`, `1439-1457`)
- **Mechanizm:**
  1. Przenieść odczyt `QUERY_CACHE` ([`fwd.rs:1439-1457`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1439-L1457)) przed `lock_state(inst)` w `forward_inner` (obok `descriptor_lockfree` w [`fwd.rs:917`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L917)) i dodać do `query_cacheable` ([`fwd.rs:698`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L698)): `vkGetDescriptorSetLayoutBindingOffsetEXT` (2 695 obiegów) oraz `vkGetDescriptorSetLayoutSizeEXT` (220 obiegów).
  2. Dodać `vkBeginCommandBuffer` i `vkResetCommandBuffer` do szybkiej ścieżki `DispObj::pending`: w `fast_record` dla `Begin`/`Reset` najpierw wyczyścić bufor `(*cb).pending` pod jego własnym mutexem, a następnie zakodować samo `vkBeginCommandBuffer`/`vkResetCommandBuffer` jako pierwszy element `pending`. Dzięki temu `Begin`/`Reset` nie wchodzą pod globalną blokadę `InstState`.
- **Warunek poprawności:** `vkBeginCommandBuffer` i `vkResetCommandBuffer` zwracają `VK_SUCCESS (0)`, nie tworzą nowych uchwytów i dotyczą wyłącznie jednego `VkCommandBuffer` (zewnętrznie synchronizowanego przez aplikację). Gdy pierwszy `vkCmd*` spoza `fast_table` lub `vkEndCommandBuffer` wywoła `drain_cb` ([`fwd.rs:951, 957`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L951)), `vkBeginCommandBuffer` trafi do `st.batch` dokładnie przed wszystkimi swoimi `vkCmd*`.

---

## 3. Co zweryfikować pomiarem przed implementacją

1. **Test flagi `GPU_ICD_NO_REFRESH_UNCACHED=1` (+ ewentualnie `GPU_ICD_LAZY_ALL=1`) w Rise of the Tomb Raider (bez pisania kodu, 2 minuty):**
   - W [`fwd.rs:1468`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1468) flaga `GPU_ICD_NO_REFRESH_UNCACHED=1` wyłącza odświeżanie pamięci bez `HOST_CACHED` nawet poniżej `REFRESH_ALWAYS_BELOW` (16 MB).
   - **Co sprawdzić w logu:** czy `odświeżenia pamięci` spadają z `53 572` zakresów do `0` (bo jedyna pamięć `HOST_CACHED` `0x10165d` jest już w `lazymap`), o ile spada średni czas `WaitSemaphores` (z 5,13 ms do oczekiwanych ~2,0–2,5 ms) i czy obraz oraz fizyka w grze pozostają w 100% poprawne.
2. **Rozbicie stopera `Pending` w `fwd.rs:1584-1679` na 3 pod-czasy (5 linii diagnostycznych):**
   - Zmierzyć osobno:
     - `t_net_wait`: sam czas `p.receive()` ([`fwd.rs:993`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L993)),
     - `t_reacquire_lock`: czas czekania na `lock_state(inst)` po `p.receive()` ([`fwd.rs:995-996`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L995-L996)),
     - `t_finish_deltas`: czas `apply_delta`/`drain_delta` w `exec_finish` ([`fwd.rs:1662-1675`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L1662-L1675)).
   - Pozwoli to przed budową drugiego połączenia potwierdzić co do mikrosekundy, ile z 5,13 ms znika po samym wyłączeniu zbędnych `MemReadDelta*` (Zmiana 1) i wyjęciu LZ4 spod blokady (Zmiana 2).
3. **Histogram wejść w `lock_state(inst)` wg nazwy polecenia (`LOCK_ENTRIES_BY_CMD`):**
   - Dodać licznik top-10 poleceń wchodzących w `lock_state` w [`fwd.rs:927`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L927), żeby widzieć dokładny udział `vkBeginCommandBuffer`, `vkEndCommandBuffer`, `vkQueueSubmit` i `fast_record` (próg 256 KB w [`fwd.rs:874`](file:///home/wasyl/Projects/gpu-hub/crates/gpu-icd/src/fwd.rs#L874)) w 259 900 wejściach.
4. **Sprawdzenie liczby unikalnych `VkFormat` dla `UNIFORM_TEXEL_BUFFER` (typ 4) w `vkGetDescriptorEXT`:**
   - Wypisać w `descsynth::input_of` wartości pola `format` (`*(info_ptr.add(24) as *const u32)` w `VkDescriptorAddressInfoEXT`) dla `ty == 4` i `ty == 5`, aby potwierdzić, że kilka formatów pokrywa wszystkie 2 652 chybienia.
