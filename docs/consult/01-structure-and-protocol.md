> **Źródło: DeepSeek V4.1 Flash (lokalnie, `.2`), konsultacja część 1, 2026-11-01. Nieweryfikowane — sugestie, nie fakty.**
> **Przyjęto jako domyślny kierunek (polecenie użytkownika). Odchylenia uzasadnione dowodem:**
> 1. *Uchwyty dispatchable* (VkInstance, VkPhysicalDevice, VkDevice, VkQueue, VkCommandBuffer) muszą być wskaźnikami do struktur zaczynających się od danych loadera (`VK_LOADER_DATA`, interfejs ICD loadera Vulkan); `u64 kind|client_id|counter` z §2.3 to identyfikator w protokole (wire-id), a ICD zwraca adres lokalnej struktury `{ loader_data, wire_id }`. Non-dispatchable mogą być zwracane bezpośrednio.
> 2. Lista rodzajów uchwytów w §2.3 jest niepełna (brak m.in. VkPipelineCache): `kind` generować z 63 typów uchwytów w vk.xml, nie ręcznie.

# SEKCJA 1 — Struktura programu

## 1.1 Drzewo crate'ów

```
gpu-remote/                     workspace (resolver = "2")
├── gpu-vkgen/                  [build-time] generator z vk.xml
├── gpu-wire/                   [shared]  ramki, kodeki, kompresja, kanały
├── gpu-proto/                  [shared]  typy komunikatów (generowane + ręczne)
├── gpu-shadow/                 [client]  pamięć cieniowa, mprotect, userfaultfd
├── gpu-icd/                    [client]  cdylib: interfejs ICD Vulkan
├── gpu-client/                 [client]  logika klienta (poza ICD, testowalna)
├── gpu-renderd/                [server]  demon, replay, mapa uchwytów
├── gpu-video/                  [server]  kodek (NVENC/VA-API) + enkoder
└── gpu-tools/                  [dev]     vkxml-dump, wire-tap, bench
```

Zależności zewnętrzne (wszystkie MIT/Apache-2.0/BSD): `zstd` (BSD), `libc` (MIT/Apache), `memmap2` (MIT/Apache), `crossbeam-channel` (MIT/Apache), `parking_lot` (MIT/Apache), `tracing` (MIT). Bez `ash`, bez `vulkano`, bez `venus`.

## 1.2 `gpu-vkgen` — generator

Wejście: `vk.xml` (pinned commit, hash w `build.rs`). Wyjście: `gpu-proto::generated` (typy poleceń, struktury, kodery/dekodery, tabele dyspozycji) oraz `gpu-icd::dispatch` (tabela `VkInstance`/`VkDevice`).

```rust
// gpu-vkgen/src/lib.rs
pub struct Registry { /* commands, structs, handles, enums */ }
pub fn parse(xml: &str) -> Result<Registry, ParseError>;
pub fn classify(cmd: &Command) -> CommandClass;   // FireAndForget | DeferredResult | OptimisticCreate | Query | Sync
pub fn emit_proto(reg: &Registry, out: &mut impl Write) -> Result<(), EmitError>;
pub fn emit_dispatch(reg: &Registry, out: &mut impl Write) -> Result<(), EmitError>;
```

Reguła klasyfikacji jest deterministyczna (na podstawie sygnatury w `vk.xml`), a wynik jest zamrożony w `generated/classes.rs` i sprawdzany testem — zmiana `vk.xml` bez zmiany klasyfikacji wywala CI.

## 1.3 `gpu-wire` — transport

```rust
pub const WIRE_VERSION: u16 = 1;
pub const MAX_FRAME: u32 = 1 << 20;

pub struct FrameHeader {           // 16 B, little-endian
    pub len: u32,                  // payload po nagłówku
    pub stream: u8,                // 0=Control 1=Cmd 2=Mem 3=Video 4=Query
    pub flags: u8,                 // bit0=zstd, bit1=last-in-batch, bit2=ack-req
    pub seq: u16,                  // per-stream, modulo 65536
    pub epoch: u32,                // inkrementowany przy reconnect
    pub crc32c: u32,               // opcjonalny (flag bit3), domyślnie 0
}

pub trait Codec { fn encode(&self, w: &mut Vec<u8>); fn decode(r: &mut Reader) -> Result<Self, WireError>; }
pub struct Reader<'a> { /* bounds-checked */ }
pub struct Writer { /* growable */ }

pub struct ZstdCtx { /* streaming encoder/decoder, per-stream */ }
pub fn compress(ctx: &mut ZstdCtx, level: i32, src: &[u8], dst: &mut Vec<u8>);
```

`Reader` zwraca `WireError::Truncated` przy każdej próbie czytania poza buforem — to jest jedyna ścieżka błędu dekodowania, reszta to `Malformed`.

## 1.4 `gpu-proto` — komunikaty

```rust
// ręczne, niegenerowane
pub enum Stream { Control = 0, Cmd = 1, Mem = 2, Video = 3, Query = 4 }

pub struct Hello { pub version: u16, pub client_id: u64, pub features: u64 }
pub struct HelloAck { pub version: u16, pub server_id: u64, pub features: u64, pub snapshot_len: u32 }
pub struct Snapshot { /* cała migawka właściwości urządzenia, zstd */ }

pub struct Handle { pub raw: u64 }   // nadawany przez klienta, patrz §2.3
pub struct Batch { pub cmds: Vec<Cmd> }
pub struct Cmd { pub op: u16, pub payload: Vec<u8> }  // op = indeks w tabeli vk.xml

pub struct MemDiff { pub handle: Handle, pub offset: u64, pub len: u32, pub data: Vec<u8> }
pub struct MemInvalidate { pub handle: Handle, pub offset: u64, pub len: u32 }

pub struct Submit { pub queue: Handle, pub submits: Vec<SubmitInfo>, pub fence: Option<Handle> }
pub struct Present { pub queue: Handle, pub swapchain: Handle, pub image_index: u32, pub frame_id: u64 }
```

## 1.5 `gpu-icd` — fałszywy sterownik

Eksportuje wyłącznie interfejs ICD (`vk_icdGetInstanceProcAddr`, `vk_icdGetInstanceProcAddrLunar`, `vk_icdNegotiateLoaderICDInterfaceVersion`). Każda funkcja `vk*` z tabeli dyspozycji to cienka nakładka:

```rust
#[no_mangle] pub extern "C" fn vk_icdGetInstanceProcAddr(
    instance: VkInstance, name: *const c_char) -> PFN_vkVoidFunction;

// wewnętrznie, dla każdego polecenia generowane:
pub unsafe extern "C" fn vkCreateBuffer(
    device: VkDevice, info: *const VkBufferCreateInfo,
    alloc: *const VkAllocationCallbacks, out: *mut VkBuffer) -> VkResult;
```

Ciało `vkCreateBuffer` (OptimisticCreate):
1. `Handle::next()` → `raw: u64` (monotoniczny, atomowy licznik).
2. `proto::Cmd { op, payload }` zakodowany z `info` (wskaźniki `pNext` rekurencyjnie).
3. `client.cmd_tx.send(cmd)` (kanał bez blokowania, `crossbeam` bounded 4096).
4. `*out = raw as VkBuffer`; zwróć `VK_SUCCESS`.

Ciało `vkCmdDraw` (FireAndForget): zakoduj i wyślij do tego samego kanału — **bez** czekania. Ciało `vkQueueSubmit` (Sync): zakoduj, wyślij, wstaw `fence` do `pending`, wróć `VK_SUCCESS`; faktyczne czekanie dopiero w `vkWaitForFences`/`vkGetFenceStatus`.

## 1.6 `gpu-client` — logika klienta

```rust
pub struct Client {
    pub cmd_tx: Sender<Cmd>,        // → wątek nadawczy
    pub mem_tx: Sender<MemDiff>,    // → wątek nadawczy (osobny priorytet)
    pub query_tx: Sender<Query>,    // → wątek zapytań
    pub recv_rx: Receiver<ServerMsg>, // ← wątek odbiorczy
    pub shadow: ShadowTable,
    pub handles: HandleTable,
}

pub struct ShadowTable { /* HashMap<Handle, ShadowRegion> */ }
pub struct ShadowRegion {
    pub local: *mut u8,        // memfd mmap, oddany aplikacji
    pub len: usize,
    pub dirty: BitVec,         // 4 KiB strony
    pub guard: bool,           // mprotect PROT_READ aktywny
}

impl ShadowTable {
    pub fn map(&mut self, h: Handle, len: usize) -> Result<*mut u8, ShadowError>;
    pub fn unmap(&mut self, h: Handle) -> Result<(), ShadowError>;
    pub fn flush(&mut self, h: Handle, range: Range<u64>) -> Vec<MemDiff>;
    pub fn invalidate(&mut self, h: Handle, range: Range<u64>) -> Result<(), ShadowError>;
}
```

Wątki klienta (patrz §1.9): **wątek aplikacji** (wywołania `vk*`), **wątek nadawczy** (partie + zstd), **wątek odbiorczy** (dekodowanie, rozdzielanie do `recv_rx`), **wątek prezentacji** (dekoder VA-API → `vkQueuePresentKHR` lokalnego GPU). `SIGSEGV` obsługiwany w wątku aplikacji (handler asynchroniczny, tylko `mprotect` + `siglongjmp` do trampoliny — bez alokacji).

## 1.7 `gpu-renderd` — serwer

```rust
pub struct Server {
    pub listener: TcpListener,
    pub vk: VkBackend,             // dynamicznie ładowany libvulkan.so.1
    pub handles: ServerHandleTable, // Handle → VkBuffer/VkImage/...
    pub sessions: HashMap<u64, Session>,
}

pub struct Session {
    pub cmd_rx: Receiver<Batch>,
    pub mem_rx: Receiver<MemDiff>,
    pub video_tx: Sender<EncodedFrame>,
    pub replay: Replayer,
}

impl Replayer {
    pub fn apply(&mut self, batch: Batch) -> Result<(), ReplayError>;
    pub fn apply_mem(&mut self, diff: MemDiff) -> Result<(), ReplayError>;
    pub fn present(&mut self, p: Present) -> Result<(), ReplayError>;
}
```

Wątki serwera: **akceptujący**, **per-sesja: odbiorczy** (dekodowanie ramek), **per-sesja: replay** (wykonanie na prawdziwym Vulkanie, jeden wątek — Vulkan nie lubi wielowątkowego dostępu do tego samego `VkDevice`), **per-sesja: enkoder wideo** (osobny wątek, sprzętowy kodek).

## 1.8 `gpu-shadow` — pamięć cieniowa

```rust
pub struct Shadow { /* per-region */ }
pub fn install_sigsegv_handler();
pub fn protect(ptr: *mut u8, len: usize) -> Result<(), ShadowError>;   // mprotect PROT_READ
pub fn unprotect(ptr: *mut u8, len: usize) -> Result<(), ShadowError>;
pub fn dirty_pages(region: &ShadowRegion) -> impl Iterator<Item = u64>;
```

Alternatywa `userfaultfd` (Linux ≥ 5.11, `UFFDIO_WRITEPROTECT`) — wybierzemy po benchmarku w M3; API `Shadow` jest wspólne.

## 1.9 Przepływ danych — kto kogo woła

```
[app thread]  vkCmdDraw ──► gpu-icd ──► cmd_tx (bounded 4096, non-blocking)
                                        │
[app thread]  vkQueueSubmit ──► gpu-icd ──► cmd_tx + flush shadow (mem_tx)
                                        │
[sender thread]  cmd_rx + mem_rx ──► batch (≤ 64 KiB) ──► zstd ──► TCP ──►
                                        │
[server recv thread]  TCP ──► frame ──► cmd_rx/mem_rx ──► replay thread ──► Vulkan
                                        │
[server encoder thread]  swapchain image ──► NVENC ──► video_tx ──► TCP ──►
                                        │
[client recv thread]  TCP ──► frame ──► recv_rx ──► dispatch (query/fence/video)
                                        │
[present thread]  video frame ──► VA-API decode ──► vkQueuePresentKHR (local GPU)
```

Kolejki: `cmd_tx` (bounded 4096, `try_send` — przy pełnej kolejce blokujemy aplikację, to jest backpressure), `mem_tx` (bounded 256, priorytet wyższy niż `cmd_tx`), `query_tx` (bounded 64), `recv_rx` (bounded 1024). Wszystkie kanały `crossbeam::channel`.

---

# SEKCJA 2 — Protokół przewodowy

## 2.1 Strumienie

Jeden TCP (port 7443, TLS 1.3 opcjonalnie w M8). Wewnątrz **pięć logicznych strumieni** multipleksowanych w jednym połączeniu (żeby uniknąć head-of-line blocking przy dużych partiach wideo — patrz §2.7):

| ID | Nazwa | Kierunek | Zawartość |
|---|---|---|---|
| 0 | Control | ↔ | Hello, Snapshot, błędy, keepalive, negocjacja |
| 1 | Cmd | → | partie poleceń Vulkan |
| 2 | Mem | ↔ | `MemDiff`, `MemInvalidate` |
| 3 | Video | ← | zakodowane klatki (NVENC/VA-API) |
| 4 | Query | ↔ | zapytania dynamiczne + odpowiedzi |

Każdy strumień ma własny licznik `seq: u16` (mod 65536) i własny kontekst zstd. Kolejność **w obrębie strumienia** jest gwarantowana; **między strumieniami** — nie, z wyjątkiem par zależnych (patrz §2.4).

## 2.2 Ramkowanie

```
offset  size  field
0       4     len        (u32 LE, długość payloadu, ≤ MAX_FRAME)
4       1     stream     (u8)
5       1     flags      (u8: bit0=zstd, bit1=last-in-batch, bit2=ack-req, bit3=crc)
6       2     seq        (u16 LE)
8       4     epoch      (u32 LE)
12      4     crc32c     (u32 LE, 0 jeśli bit3=0)
16      len   payload
```

Nagłówek 16 B. Payload po dekompresji to sekwencja `Cmd`/`MemDiff`/itd. z własnym `len` (LEB128). `epoch` inkrementowany przy każdym reconnect — serwer odrzuca ramki ze starym `epoch` (stan sesji i tak jest odtwarzany od zera, patrz §2.8).

## 2.3 Identyfikatory uchwytów

Klient nadaje **wszystkie** uchwyty (trik 3: OptimisticCreate). Format:

```
raw: u64 = (kind: u8) << 56 | (client_id: u16) << 40 | (counter: u40)
```

- `kind`: 1=Instance, 2=PhysicalDevice, 3=Device, 4=Queue, 5=Buffer, 6=Image, 7=ImageView, 8=Sampler, 9=DescriptorSet, 10=DescriptorSetLayout, 11=Pipeline, 12=PipelineLayout, 13=CommandBuffer, 14=CommandPool, 15=Fence, 16=Semaphore, 17=Event, 18=QueryPool, 19=Swapchain, 20=RenderPass, 21=Framebuffer, 22=ShaderModule, 23=BufferView, 24=DeviceMemory, 25=Surface, 26=DescriptorPool, 27=DescriptorUpdateTemplate, 28=SamplerYcbcrConversion.
- `client_id`: losowy przy `Hello`, zapobiega kolizji po reconnect.
- `counter`: monotoniczny, atomowy.

Serwer trzyma `HashMap<u64, VkHandle>` i tłumaczy 1:1. Uchwyty z `kind` niezgodnym z oczekiwanym typem w danym polu → `ReplayError::HandleKindMismatch` (błąd protokołu, nie Vulkan).

**Wyjątek:** `VkPhysicalDevice` — klient nie tworzy go, dostaje z migawki (jeden uchwyt na fizyczne urządzenie serwera, `kind=2`). `VkInstance` — klient tworzy lokalnie, serwer tworzy własny, mapowanie przez `kind=1`.

## 2.4 Gwarancje kolejności

- W obrębie strumienia: ścisła kolejność `seq`.
- `Cmd` przed `Mem` dla tego samego uchwytu: **wymagane**. Realizacja: `MemDiff` niesie `after_seq: u16` (numer ostatniego `Cmd` w strumieniu 1, który musi być wykonany przed tą różnicą). Serwer buforuje `MemDiff` do momentu wykonania `Cmd` o `seq >= after_seq`. To jest jedyny mechanizm zależności między strumieniami.
- `Query` przed `Cmd`, który od niego zależy: klient wysyła `Query` z `dep_seq` (numer `Cmd`, po którym odpowiedź jest ważna); serwer odpowiada, gdy `replay_seq >= dep_seq`.
- `Video` przed `Present` lokalnym: `frame_id` monotoniczny; klient odrzuca klatki starsze niż ostatnio zaprezentowana.

## 2.5 Batching

Wątek nadawczy klienta:
1. Zbiera `Cmd` z `cmd_rx` do bufora ≤ **64 KiB** (payload przed kompresją) lub do **1 ms** (timeout, `recv_timeout`).
2. Dokleja `MemDiff` z `mem_tx` (priorytet: jeśli `mem_tx` niepusty, dołącz natychmiast, nie czekaj na timeout).
3. Serializuje: `[u32 count][Cmd...]`, każdy `Cmd` = `[u16 op][u32 len][payload]`.
4. Kompresuje zstd (`level=3` dla Cmd, `level=1` dla Mem — pamięć jest często nieściśliwa, ale nagłówek i tak się zwija).
5. Wysyła ramkę z `flags |= zstd | last-in-batch`.

Serwer dekoduje partię i wykonuje **sekwencyjnie** w wątku replay. Partia jest jednostką atomową dla błędów: jeśli `Cmd` w środku partii zwróci błąd, reszta partii jest odrzucana, sesja przechodzi w `DeviceLost` (patrz §6 projektu).

## 2.6 Kompresja

- zstd streaming, kontekst per strumień, `window_log=17` (128 KiB), `level=3` (Cmd), `level=1` (Mem), `level=1` (Video — ale wideo i tak jest już skompresowane przez NVENC, więc zstd tylko dla metadanych).
- Brak kompresji, jeśli payload < 256 B (narzut większy niż zysk).
- `flags bit0` mówi, czy payload jest skompresowany — serwer nie zakłada.

## 2.7 Kontrola przepływu

- TCP backpressure jest podstawą: `try_send` na `cmd_tx` zwraca `Full`, wtedy `vk*` blokuje wątek aplikacji (jak prawdziwy sterownik przy pełnej kolejce).
- **Okno wideo:** serwer wysyła `Video` tylko gdy klient potwierdził odbiór poprzedniej klatki (`ack-req` w `flags bit2`, klient odpowiada `Ack { frame_id }` na strumieniu Control). Maks. 2 klatki w locie — inaczej rośnie opóźnienie.
- **Keepalive:** Control co 500 ms, timeout 2 s → reconnect.
- **Reconnect:** nowy `epoch`, nowy `Hello`, serwer odtwarza stan z ostatniego `Snapshot` + replay od zera (stan gry jest po stronie klienta, więc to jest bezpieczne — ale tracimy GPU-side state, więc klient musi odtworzyć wszystkie uchwyty; to jest świadomy kompromis M2).

## 2.8 Wersjonowanie i negocjacja

`Hello { version: u16, client_id: u64, features: u64 }` — `version` to `WIRE_VERSION`. Serwer odpowiada `HelloAck { version, server_id, features, snapshot_len }`. Zgodność: `major` musi się zgadzać, `minor` — serwer przyjmuje `<=` swojego. `features` to bitmaska:

| bit | feature |
|---|---|
| 0 | `SNAPSHOT_V1` (migawka właściwości) |
| 1 | `MEM_DIFF` (pamięć cieniowa) |
| 2 | `VIDEO_NVENC` |
| 3 | `VIDEO_VAAPI` |
| 4 | `SPECULATIVE_QUERY` |
| 5 | `ZSTD_DICT` (słownik trenowany na `vk.xml` — opcjonalny) |

Rozszerzenia Vulkana: klient wysyła listę `VkExtensionProperties` z migawki; serwer odfiltrowuje te, których nie ma w swoim `vkEnumerateDeviceExtensionProperties`, i odsyła w `HelloAck` (jako `Snapshot` z polem `extensions`). Klient **nie** reklamuje rozszerzeń, których serwer nie ma — to jest jedyna asymetria w migawce.

---

## 2.9 Przykładowy przebieg — `vkCreateBuffer`

```
app: vkCreateBuffer(dev, &info, NULL, &buf)
  icd: h = Handle::next(kind=Buffer)          // np. 0x0500_0000_0000_0001
  icd: payload = encode(info)                 // pNext łańcuch rekurencyjnie
  icd: cmd_tx.send(Cmd { op: OP_vkCreateBuffer, payload })
  icd: *buf = h as VkBuffer
  icd: return VK_SUCCESS

[sender thread, po ≤1 ms]
  batch = [Cmd{op, payload}]
  frame: len=..., stream=1, flags=zstd|last, seq=42, epoch=1
  → TCP

[server replay thread]
  decode → Cmd{op=OP_vkCreateBuffer}
  VkBuffer real; vkCreateBuffer(dev, &info, NULL, &real)
  handles.insert(h, real)
  // brak odpowiedzi do klienta (OptimisticCreate)
```

## 2.10 Przykładowy przebieg — `vkCmdDraw` w partii

```
app: vkCmdBeginRenderPass(cb, ...)   → Cmd{op=OP_vkCmdBeginRenderPass}
app: vkCmdBindPipeline(cb, ...)      → Cmd{op=OP_vkCmdBindPipeline}
app: vkCmdDraw(cb, 3, 1, 0, 0)       → Cmd{op=OP_vkCmdDraw, payload=[3,1,0,0]}
app: vkCmdEndRenderPass(cb)          → Cmd{op=OP_vkCmdEndRenderPass}

[sender thread]
  batch = [4 Cmd], payload ~120 B
  frame: stream=1, flags=zstd|last, seq=43
  → TCP

[server replay thread]
  decode 4 Cmd, wykonaj po kolei na realnym VkCommandBuffer
  // brak odpowiedzi
```

## 2.11 Przykładowy przebieg — `vkQueueSubmit` + `vkWaitForFences`

```
app: vkQueueSubmit(q, 1, &submit, fence)
  icd: h_fence = Handle::next(kind=Fence)
  icd: Cmd{op=OP_vkQueueSubmit, payload=[q, submit_info, h_fence]}
  icd: pending.insert(h_fence, PendingFence { seq: 44 })
  icd: return VK_SUCCESS

app: vkWaitForFences(dev, 1, &h_fence, VK_TRUE, UINT64_MAX)
  icd: recv_rx.recv() // blokuje wątek aplikacji
       // serwer po wykonaniu submit wysyła:
       //   stream=4, Query{ kind: FenceSignaled, handle: h_fence, seq: 44 }
  icd: return VK_SUCCESS

[serwer]
  replay: vkQueueSubmit(real_q, ...)
  gdy fence signaled (osobny wątek poll lub vkWaitForFences na serwerze):
    query_tx.send(Query{ FenceSignaled, h_fence, seq: 44 })
```

**Uwaga:** `vkWaitForFences` blokuje wątek aplikacji — to jest zgodne z Vulkanem i akceptowalne, bo gra i tak czeka na GPU. Pipeline klatek (M6) ukryje to przez spekulację: serwer wysyła `FenceSignaled` z wyprzedzeniem, gdy tylko fence jest sygnalizowany, a klient buforuje odpowiedzi.

---

## 2.12 Co jeszcze trzeba dopiąć przed M1

1. **Snapshot właściwości** — pełna lista `VkPhysicalDeviceProperties`, `VkPhysicalDeviceMemoryProperties`, `VkPhysicalDeviceFeatures`, `VkQueueFamilyProperties`, `VkExtensionProperties`. Rozmiar ~4 KiB, zstd → ~1 KiB. Wysyłany raz przy `Hello`.
2. **`pNext` łańcuchy** — generator musi rekurencyjnie kodować `sType` + wskaźnik; nieznane `sType` → `WireError::UnknownStruct` (klient nie reklamuje rozszerzeń, których serwer nie zna).
3. **`VkAllocationCallbacks`** — zawsze `NULL` w protokole (klient alokuje lokalnie, serwer używa domyślnych).
4. **`VkDeviceMemory`** — `kind=24`, ale mapowanie przez `ShadowTable` (patrz §1.6); `vkMapMemory` zwraca wskaźnik do lokalnego `memfd`, nie do serwera.
5. **Błędy** — `Control` niesie `Error { code: VkResult, handle: Option<Handle>, msg: String }`; przy `VK_ERROR_DEVICE_LOST` klient przechodzi w stan terminalny (zgodnie z §6 projektu).