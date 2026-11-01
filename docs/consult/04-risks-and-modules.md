> **Źródło: DeepSeek V4.1 Flash (lokalnie, `.2`), konsultacja część 4, 2026-11-01. Nieweryfikowane. Odpowiedź ucięta limitem tokenów na końcu §8 (M12).**
> **Uwagi recenzji:** ryzyka R1 (pamięć cieniowa) rozstrzygamy już zmierzonym `WP_ASYNC`+`PAGEMAP_SCAN` (`docs/experiments/`); w kolejności ryzyka dodatkowo wysoko stawiamy R3 (ICD przez loader i Proton/DXVK) i R6 (poprawność grupowania poleceń). Moduły z §8 zlecać wg zasady: kontrakt testowy (w tym złośliwe dane z sieci) napisany PRZED zleceniem, ściągawka API bibliotek w prompcie, przegląd kodu po zleceniu (modele lokalne zmyślają API i pomijają wartości brzegowe).

# SEKCJA 7 — Dziesięć najbardziej ryzykownych założeń

Kolejność = malejące ryzyko. „Koszt" w godzinach pracy jednej osoby; eksperymenty 1–4 robimy **przed** M2.

---

### R1. Pamięć cieniowa (userfaultfd + PAGEMAP_SCAN) wytrzyma obciążenie realnej gry
**Dlaczego ryzykowne:** zmierzone 1,78 µs/stronę to *średnia* dla pojedynczego wątku. DXVK/Proton generuje dziesiątki tysięcy mapowań i setki tysięcy stron na klatkę; userfaultfd serializuje obsługę faultów na wątku obsługi, a PAGEMAP_SCAN na 4 KiB stronach przy 8 GB/s to ~2 mln skanów/s. Jeśli obsługa faultów wpadnie w wątek gry, dostajemy 5–15 ms zacięć. Dodatkowo `UFFD_USER_MODE_ONLY` + `WP_ASYNC` to świeża ścieżka jądra — mogą być regresje między 6.7 a 6.12.
**Eksperyment:** harness w Rust: N wątków zapisuje do 2 GiB memfd z WP_ASYNC, wątek obsługi robi PAGEMAP_SCAN i kopiuje dirty pages do bufora. Mierzymy: p99 opóźnienia zapisu w wątku roboczym, przepustowość skanowania, CPU%. **Sukces:** p99 < 200 µs, ≥ 6 GB/s skanowania, brak wzrostu opóźnień przy 8 wątkach. **Porażka:** p99 > 1 ms lub skalowanie liniowo spada.
**Koszt:** 12 h.
**Gdy porażka:** fallback na (a) śledzenie dirty przez `mprotect(PROT_READ)` + SIGSEGV handler z rejestracją stron w bitmapie per-VMA (droższe, ale przewidywalne), (b) dla buforów z `HOST_VISIBLE` — jawne `vkFlushMappedMemoryRanges` przechwycone w ICD i wysyłanie zakresów zamiast skanowania. To zmienia kontrakt M3, nie architekturę.

---

### R2. Sprzętowa kompresja wideo (NVENC HEVC LL) zmieści się w 400 Mb/s przy jakości akceptowalnej dla AAA
**Dlaczego ryzykowne:** 400 Mb/s na 1440p60 to ~0,9 bpp — dla HEVC to granica. Sceny z ruchem kamery, deszcz, cząstki → bloki, smużenie, „mydło". NVENC LL ma ograniczony lookahead; nie ma tu miejsca na 2-pass. Do tego enkoder zajmuje zasoby SM na 3090 Ti (wpływ na render).
**Eksperyment:** offline: 3 klipy 1440p60 z gier (Cyberpunk, Doom Eternal, symulator lotu) → NVENC HEVC LL, CBR 400 Mb/s, GOP 1, `-tune ll`, `-preset p1..p4`. Mierzymy VMAF (libvmaf, BSD-2) i subiektywnie na 3 osobach. **Sukces:** VMAF ≥ 92 w p1, ≥ 95 w p4, brak widocznych artefaktów w ruchu. **Porażka:** VMAF < 88 lub „mydło" w ruchu.
**Koszt:** 8 h.
**Gdy porażka:** (a) adaptacyjny bitrate per-klatka (sceny statyczne 150 Mb/s, ruchome 600 Mb/s, średnia ≤ 400), (b) AV1 na NVENC (Ada+ — 3090 Ti to Ampere, **nie ma** AV1 encode; sprawdzić!), (c) renderowanie w 1080p i upscaling FSR na kliencie — ale to zmienia kontrakt „wynik = pełna klatka".

---

### R3. Fałszywy sterownik (ICD) przejdzie przez loader Vulkana i Proton/DXVK bez łamania założeń
**Dlaczego ryzykowne:** loader Vulkana robi `dlsym` na ~200 symboli, sprawdza `vkEnumerateInstanceVersion`, `vkGetInstanceProcAddr` z łańcuchami, `VK_KHR_get_physical_device_properties2`, extensiony. DXVK wymaga konkretnych extensionów (`VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf`, `VK_KHR_image_format_list`, `VK_EXT_robustness2`...). Każdy brak = crash lub fallback do software. Uchwyty dispatchable muszą być prawdziwymi wskaźnikami (już ustalone), ale DXVK robi też `vkGetDeviceProcAddr` na funkcje, których nie zadeklarowaliśmy.
**Eksperyment:** minimalny ICD (cdylib) eksportujący tylko `vk_icdGetInstanceProcAddr` + `vk_icdNegotiateLoaderICDInterfaceVersion`, zwracający stub-y dla 20 funkcji; uruchamiamy `vulkaninfo` i `DXVK_HUD=1 wine game.exe` (prosta gra DX11). **Sukces:** vulkaninfo przechodzi, DXVK inicjalizuje device, gra renderuje cokolwiek (choćby czarny ekran bez crasha). **Porażka:** loader odrzuca ICD lub DXVK abortuje na brakującym symbolu.
**Koszt:** 16 h.
**Gdy porażka:** (a) generujemy pełny stub z vk.xml (mamy parser!) — wszystkie 865 poleceń zwraca `VK_SUCCESS`/no-op, dopiero potem podmieniamy na wire; (b) jeśli loader wymaga czegoś nietypowego — patch loadera (MIT) lub własny `LD_PRELOAD` przed loaderem. To ostatnia deska ratunku, bo łamie „bez opierania się na cudzych projektach".

---

### R4. Opóźnienie end-to-end (input → foton) < 50 ms przy RTT 0,55 ms
**Dlaczego ryzykowne:** RTT kabla to 0,55 ms, ale dochodzi: enqueue w grze, symulacja, render (16 ms), NVENC (2–5 ms), sieć, dekoder VA-API (2–4 ms), compositor klienta (1–2 klatki). Realistycznie 3–4 klatki opóźnienia = 50–67 ms. Cel „płynna gra AAA" w strzelankach wymaga < 40 ms. Trik 3 („odpowiedzi z wyprzedzeniem") pomaga tylko na stan, nie na obraz.
**Eksperyment:** end-to-end na M5: timestamp w inputcie (klient) → timestamp w klatce wyjściowej (serwer, wstrzyknięty w metadane) → timestamp prezentacji (klient). Mierzymy percentyle. **Sukces:** p50 < 45 ms, p99 < 70 ms. **Porażka:** p50 > 60 ms.
**Koszt:** 10 h (po M5).
**Gdy porażka:** (a) reprojekcja czasowa na kliencie (klient przesuwa obraz o różnicę pozycji kamery z ostatniego inputu — wymaga wysyłania macierzy widoku per klatka, ~64 B), (b) renderowanie w 120 Hz na serwerze i interpolacja na kliencie, (c) obniżenie rozdzielczości + FSR. Reprojekcja to największa dźwignia, ale to nowy moduł.

---

### R5. Kompresja różnicowa danych (trik 2) da realny zysk na buforach wierzchołków/indeksów
**Dlaczego ryzykowne:** gry często piszą bufory jednorazowo (statyczne siatki) — tam różnice = pełne dane. Dynamiczne bufory (uniform, instancje) są małe, ale zmieniają się co klatkę w całości. Zysk z delty jest duży tylko dla dużych, rzadko zmienianych buforów. Jeśli gra alokuje 2 GiB i wypełnia raz, pierwsza klatka to 2 GiB przez sieć — przy 1 Gb/s = 16 s.
**Eksperyment:** przechwyć realny trace z DXVK (M2, tryb pętli lokalnej) dla 3 gier, 300 klatek. Analiza: rozkład rozmiarów `vkCmdUpdateBuffer`/`vkCmdCopyBuffer`, % bajtów zmienionych per bufor per klatka. **Sukces:** ≥ 70% bajtów w strumieniu to delty ≤ 10% rozmiaru. **Porażka:** > 50% to pełne zapisy.
**Koszt:** 6 h (analiza offline po M2).
**Gdy porażka:** (a) strumieniowanie zasobów z serwera na żądanie (serwer ma pełny stan, klient tylko czyta — ale to łamie „gra działa lokalnie"), (b) pre-fetch: klient wysyła „potrzebuję bufora X" zanim gra go użyje (wymaga przewidywania — heurystyka z poprzednich klatek), (c) kompresja bezstratna (zstd, MIT) na pełnych zapisach — 2–3× zysk.

---

### R6. Grupowanie poleceń (trik 1) bez czekania na odpowiedź nie zepsuje synchronizacji
**Dlaczego ryzykowne:** Vulkan ma ścisłe zależności: `vkWaitSemaphores`, `vkGetFenceStatus`, `vkQueueSubmit` z fence'ami. Jeśli klient wyśle 100 poleceń bez czekania, a gra zrobi `vkWaitForFences`, musimy albo zablokować do odpowiedzi serwera (RTT 0,55 ms × liczba sync = katastrofa), albo zgadywać. Zgadywanie = błędy renderu lub deadlock. Trik 3 („odpowiedzi z wyprzedzeniem") zakłada, że serwer odpowie zanim klient zapyta — ale to wymaga predykcji stanu.
**Eksperyment:** symulator: 10 000 losowych sekwencji poleceń z vk.xml (mamy klasyfikację!), z sync co N. Mierzymy: ile razy klient musi czekać (round-trip), ile razy predykcja się myli. **Sukces:** < 5% sekwencji wymaga synchronicznego round-tripu, 0 deadlocków. **Porażka:** > 20% lub jakikolwiek deadlock.
**Koszt:** 14 h.
**Gdy porażka:** (a) „optimistic submit": klient zakłada sukces fence'a, serwer wysyła korektę jeśli nie — gra dostaje fałszywy `VK_SUCCESS` i może się rozjechać (ryzykowne, ale wiele gier tego nie zauważy), (b) lokalne fence'y po stronie klienta (klient śledzi stan kolejki sam, serwer tylko potwierdza), (c) ograniczenie grupowania do poleceń bez sync (bezpieczne, mniejszy zysk).

---

### R7. VA-API na kliencie (Linux) obsłuży HEVC LL z niskim opóźnieniem na typowym laptopie
**Dlaczego ryzykowne:** VA-API na Intel/AMD ma różne wsparcie HEVC; dekodowanie LL (bez buforowania) wymaga `VASurfaceAttribLowPower` i konkretnych sterowników (iHD vs i965). Na Intel Xe jest OK, na starszych AMD (VCN 2.x) bywa 3–4 klatki opóźnienia. Laptop może mieć tylko iGPU.
**Eksperyment:** na 3 laptopach (Intel 12 gen, AMD 680M, NVIDIA Optimus) dekodujemy strumień HEVC LL z NVENC, mierzymy opóźnienie dekoder→prezentacja. **Sukces:** < 8 ms p99 na wszystkich. **Porażka:** > 15 ms na którymkolwiek.
**Koszt:** 8 h.
**Gdy porażka:** (a) software decode (libavcodec, LGPL — **uwaga na licencję!** — trzeba sprawdzić, czy LGPL przejdzie; jeśli nie, własny dekoder HEVC to kosmos), (b) H.264 zamiast HEVC (gorsza jakość przy 400 Mb/s), (c) Vulkan Video decode (nowe, ale na Intel/AMD już jest).

---

### R8. Uchwyty nadawane przez klienta (wire-id) nie kolidują z uchwytami loadera
**Dlaczego ryzykowne:** loader Vulkana wstawia własne dispatchable handle (wskaźniki do `loader_dispatchable_object`), a my nadajemy wire-id. Jeśli gra przekaże uchwyt z innej instancji lub z innego procesu (np. przez `VK_KHR_external_memory`), mapowanie się rozjedzie. DXVK tworzy wiele device'ów (per adapter) i może mieszać.
**Eksperyment:** test: 2 instancje, 2 device'y, 1000 uchwytów, losowe permutacje, w tym uchwyty „obce" (z innej instancji). Sprawdzamy, czy każdy uchwyt trafia do właściwego wire-id. **Sukces:** 0 błędów mapowania, wykrycie obcych uchwytów z czytelnym błędem. **Porażka:** kolizje lub crash.
**Koszt:** 6 h.
**Gdy porażka:** (a) wire-id = hash(wskaźnik loadera) z tabelą w ICD — ale wskaźniki się recyklują, (b) globalny rejestr uchwytów z refcountem i walidacją instancji, (c) odrzucanie obcych uchwytów z `VK_ERROR_INVALID_EXTERNAL_HANDLE`.

---

### R9. Zależności tylko MIT/Apache/BSD wystarczą (NVENC, VA-API, Vulkan headers)
**Dlaczego ryzykowne:** NVENC SDK to własna licencja NVIDIA (nie MIT/Apache/BSD!) — **to blokuje użycie NVENC SDK bezpośrednio**. VA-API to MIT, Vulkan headers to Apache-2.0, ale NVENC... trzeba sprawdzić: NVENC headers są dostępne na licencji NVIDIA (pozwalają na użycie, ale nie są MIT). Jeśli „tylko MIT/Apache/BSD" jest twarde, NVENC odpada i zostaje... software encode (za wolne) lub Vulkan Video encode (brak na Ampere).
**Eksperyment:** audyt licencji: NVENC SDK, VA-API, libvmaf, zstd, tokio, bytes. Sprawdzamy, czy NVENC SDK da się użyć (dynamiczne ładowanie `libnvidia-encode.so` przez `dlopen` — czy to obejmuje licencję?). **Sukces:** NVENC przez `dlopen` (bez linkowania, bez nagłówków w repo — własne deklaracje FFI) jest akceptowalne prawnie. **Porażka:** nie.
**Koszt:** 4 h (prawnik/audyt).
**Gdy porażka:** Vulkan Video encode (Ampere **nie ma** — sprawdzić!), software encode z niską rozdzielczością, albo... zmiana celu na kartę z AV1 (Ada). To fundamentalna zmiana, więc eksperyment robimy **pierwszy**.

---

### R10. Tryb pętli lokalnej (klient↔serwer na jednej maszynie) wiernie odwzoruje sieć
**Dlaczego ryzykowne:** pętla lokalna ma RTT ~0,01 ms i 0 utrat pakietów — nie wykryje problemów z grupowaniem, retransmisjami, reorderingiem. Możemy „zaliczyć" M2–M4 i oblać na kablu. Dodatkowo lokalna pętla nie testuje kompresji wideo (te same zasoby).
**Eksperyment:** `tc netem` na interfejsie loopback: delay 0,275 ms, jitter 0,05 ms, loss 0,01%, rate 1 Gb/s. Porównujemy metryki z prawdziwym kablem. **Sukces:** różnica < 10% w p99 opóźnienia. **Porażka:** > 30%.
**Koszt:** 4 h.
**Gdy porażka:** (a) symulator sieci w `gpu-wire` (wstrzykiwanie opóźnień/utraty w Encoder/Decoder) — tanie, deterministyczne, (b) dwa fizyczne komputery w laboratorium od M3.

---

# SEKCJA 8 — Dwanaście modułów do zlecenia słabszemu modelowi

Kolejność = zależności. Każdy moduł: crate/plik, wejście/wyjście, kontrakt testowy (testy **przed** zleceniem), pułapki, ściągawka API.

**Zasada ogólna dla wszystkich:** dane z sieci są niezaufane. Każdy dekoder musi mieć limit rozmiaru, każda długość sprawdzana przed alokacją, każdy `unwrap` na danych z wire = błąd. Testy złośliwe są obowiązkowe.

---

### M1. `gpu-proto` — ramka i nagłówek (`src/frame.rs`)
**Wejście/wyjście:**
```rust
pub struct FrameHeader { pub kind: u16, pub flags: u16, pub wire_id: u32, pub len: u32 }
pub fn encode_header(h: &FrameHeader, out: &mut [u8; 16]) -> Result<(), ProtoError>;
pub fn decode_header(buf: &[u8]) -> Result<FrameHeader, ProtoError>;
pub fn frame_len(h: &FrameHeader) -> Result<usize, ProtoError>; // waliduje len vs MAX_FRAME
```
**Testy (przed zleceniem):** `decode_header` na 0..15 bajtach → `Err(Truncated)`; `len = u32::MAX` → `Err(TooLarge)`; `len = MAX_FRAME+1` → `Err`; nieznany `kind` → `Err(UnknownKind)` (lista dozwolonych w stałej); roundtrip 10 000 losowych nagłówków; `wire_id = 0` → `Err(InvalidHandle)`; endianness (little-endian, test na znanych bajtach).
**Pułapki:** `len` jako `u32` — nie ufać, sprawdzać przed `Vec::with_capacity`; `kind` jako enum z `#[non_exhaustive]`; brak `unsafe`.
**Ściągawka:** `bytes::{Buf, BufMut}` (MIT), `thiserror` (MIT/Apache).

---

### M2. `gpu-proto` — kodek typów (`src/types.rs`)
**Wejście/wyjście:**
```rust
pub fn encode_vk_struct(def: &StructDef, src: &[u8], out: &mut Vec<u8>) -> Result<(), ProtoError>;
pub fn decode_vk_struct(def: &StructDef, src: &mut Decoder) -> Result<Vec<u8>, ProtoError>;
pub fn encode_handle(wire_id: u32, out: &mut Vec<u8>) -> Result<(), ProtoError>;
pub fn decode_handle(src: &mut Decoder) -> Result<u32, ProtoError>;
```
**Testy:** struktura z `sType`/`pNext` (łańcuch) — roundtrip; `pNext = NULL`; cykl w `pNext` → `Err` (limit głębokości 16); wskaźnik do tablicy z `count = u64::MAX` → `Err`; `count = 0, ptr != NULL` → `Err`; nieznany `sType` w `pNext` → `Err(UnknownSType)`; uchwyt `0` → `Err`; uchwyt nieznany → `Err`; unia z dyskryminatorem spoza zakresu → `Err`; roundtrip 1000 losowych struktur z vk.xml.
**Pułapki:** rekurencja w `pNext` — limit; `count * size` overflow (`checked_mul`); alignment — nie zakładać, że dane są wyrównane; `StructDef` z vk.xml może mieć pola, których nie znamy — odrzucać, nie zgadywać.
**Ściągawka:** `gpu-vkgen::StructDef`, `gpu-wire::{Encoder, Decoder}` (już gotowe — podać sygnatury).

---

### M3. `gpu-proto` — dispatch poleceń (`src/dispatch.rs`)
**Wejście/wyjście:**
```rust
pub fn encode_command(cmd: &VkCommand, out: &mut Vec<u8>) -> Result<(), ProtoError>;
pub fn decode_command(src: &mut Decoder) -> Result<VkCommand, ProtoError>;
pub fn command_id(name: &str) -> Option<u16>; // z vk.xml, stabilne
```
**Testy:** wszystkie 865 poleceń z vk.xml mają unikalny `command_id`; `decode_command` na pustym buforze → `Err`; na uciętym w środku → `Err`; nieznany `command_id` → `Err(UnknownCommand)`; polecenie z 0 argumentów; polecenie z 20 argumentami; roundtrip każdego polecenia z losowymi (ale poprawnymi typowo) argumentami; `command_id` stabilny między buildami (test na hash).
**Pułapki:** `command_id` musi być deterministyczny (sortowanie po nazwie, nie po kolejności w vk.xml); nieznane polecenie = błąd, nie „ignoruj" (może być atak).
**Ściągawka:** `gpu-vkgen` (już gotowe — podać API), `gpu-wire::Decoder`.

---

### M4. `gpu-icd` — tabela dispatch i uchwyty (`src/handles.rs`)
**Wejście/wyjście:**
```rust
pub struct HandleTable { /* ... */ }
impl HandleTable {
    pub fn insert(&self, ptr: *mut c_void) -> u32; // wire_id
    pub fn lookup(&self, wire_id: u32) -> Option<*mut c_void>;
    pub fn remove(&self, wire_id: u32) -> Option<*mut c_void>;
}
pub fn make_dispatchable(loader_ptr: *mut c_void, wire_id: u32) -> *mut c_void;
```
**Testy:** insert/lookup/remove roundtrip; lookup nieistniejącego → `None`; remove dwa razy → drugi `None`; 100 000 insertów — brak wycieku (test z `Drop`); współbieżność: 8 wątków insert/lookup (test z `loom` lub `std::thread`); `wire_id = 0` nigdy nie nadawany; recyklacja `wire_id` po remove (test: nowy insert dostaje inny id); `make_dispatchable` z `loader_ptr = null` → panika z czytelnym komunikatem (to błąd programisty, nie dane z sieci).
**Pułapki:** `wire_id` musi być `u32` i nie kolidować z 0; tabela musi być `Send + Sync`; `remove` musi być bezpieczne przy równoczesnym `lookup` (RwLock lub sharded); **nie** używać `HashMap<u32, *mut c_void>` bez `unsafe impl Send` — lepiej `Box<dyn Any>` lub typowany enum.
**Ściągawka:** `std::sync::RwLock`, `std::os::raw::c_void`, `parking_lot` (MIT/Apache) — ale tylko jeśli potrzebne.

---

### M5. `gpu-icd` — stuby funkcji Vulkan (`src/stubs.rs`, generowane z vk.xml)
**Wejście/wyjście:**
```rust
#[no_mangle] pub unsafe extern "C" fn vkCreateInstance(...) -> VkResult;
// ... 865 funkcji, każda: walidacja argumentów, pakowanie do VkCommand, wysyłka przez wire
pub fn install_dispatch(table: &mut DispatchTable);
```
**Testy:** każda funkcja z `pCreateInfo = null` → `VK_ERROR_INITIALIZATION_FAILED` (nie crash); każda funkcja z niepoprawnym uchwytem → `VK_ERROR_INVALID_EXTERNAL_HANDLE`; `vkEnumerateInstanceVersion` zwraca sensowną wersję; `vkGetInstanceProcAddr` na nieznaną nazwę → `NULL`; `vkGetDeviceProcAddr` na funkcję z innej instancji → `NULL`; test ABI: `size_of::<VkResult>() == 4`.
**Pułapki:** `extern "C"` + `#[no_mangle]` — nazwy muszą być dokładnie jak w Vulkan; **nigdy** nie panikować w `extern "C"` (panic = UB) — każda funkcja opakowana w `catch_unwind` lub ręczne sprawdzanie; `unsafe` na każdym argumencie — walidacja przed dereferencją.
**Ściągawka:** `gpu-vkgen` (generator sygnatur), `libc` (MIT), `std::panic::catch_unwind`.

---

### M6. `gpu-shadow` — pamięć cieniowa (`src/shadow.rs`)
**Wejście/wyjście:**
```rust
pub struct ShadowBuffer { /* memfd, uffd, bitmap */ }
impl ShadowBuffer {
    pub fn new(size: usize) -> Result<Self, ShadowError>;
    pub fn map(&self) -> *mut u8;
    pub fn scan_dirty(&self, out: &mut Vec<Range<usize>>) -> Result<(), ShadowError>;
    pub fn clear_dirty(&self, ranges: &[Range<usize>]);
}
```
**Testy:** `new(0)` → `Err`; `new(usize::MAX)` → `Err` (nie OOM); zapis do mapy → `scan_dirty` zwraca zakres; `clear_dirty` → kolejny `scan_dirty` pusty; równoczesny zapis i `scan_dirty` (test wyścigu); `scan_dirty` na 1 GiB — czas < 200 ms; `map` po `Drop` → UB (test compile-fail lub dokumentacja); brak wycieku fd (test z `lsof`).
**Pułapki:** `userfaultfd` wymaga uprawnień (`vm.unprivileged_userfaultfd`); `PAGEMAP_SCAN` wymaga jądra ≥ 6.7 — sprawdzać w `new` i zwracać `Err(Unsupported)`; `unsafe` przy `mmap`/`ioctl` — walidacja rozmiarów; **nie** trzymać `&mut` do mapy przez cały czas życia.
**Ściągawka:** `libc` (`memfd_create`, `mmap`, `ioctl`, `userfaultfd`, `UFFDIO_*`), `nix` (MIT) — ale `nix` może nie mieć `PAGEMAP_SCAN`, więc `libc` bezpośrednio. Podać stałe: `UFFD_USER_MODE_ONLY`, `UFFDIO_REGISTER_MODE_WP`, `PAGEMAP_SCAN`.

---

### M7. `gpu-client` — pętla sieciowa (`src/net.rs`)
**Wejście/wyjście:**
```rust
pub struct Client { /* TcpStream, Encoder, Decoder */ }
impl Client {
    pub async fn connect(addr: SocketAddr) -> Result<Self, NetError>;
    pub async fn send(&mut self, frame: &[u8]) -> Result<(), NetError>;
    pub async fn recv(&mut self) -> Result<Vec<u8>, NetError>;
    pub fn set_nodelay(&self, on: bool) -> Result<(), NetError>;
}
```
**Testy:** `recv` na zerwanym połączeniu → `Err`; `recv` na uciętej ramce (połowa nagłówka) → `Err(Truncated)`; `recv` z `len = u32::MAX` → `Err(TooLarge)` **przed** alokacją; `send` 10 000 ramek — brak deadlocka; `set_nodelay(true)` — sprawdzić `TCP_NODELAY`; reconnect po zerwaniu; test z `tokio::io::duplex` (symulacja).
**Pułapki:** `recv` musi czytać dokładnie `len` bajtów, nie więcej (ramkowanie!); limit `MAX_FRAME` (np. 16 MiB) — twardy; `nodelay` obowiązkowo (inaczej Nagle = +40 ms); backpressure — jeśli `send` blokuje, nie blokować wątku gry.
**Ściągawka:** `tokio` (MIT, `net`, `io-util`, `sync`), `bytes` (MIT).

---

### M8. `gpu-client` — bufor nadawczy z grupowaniem (`src/batch.rs`)
**Wejście/wyjście:**
```rust
pub struct Batcher { /* Vec<u8>, deadline */ }
impl Batcher {
    pub fn push(&mut self, cmd: &VkCommand) -> Result<(), BatchError>;
    pub fn flush_if_due(&mut self, now: Instant) -> Option<Vec<u8>>;
    pub fn force_flush(&mut self) -> Vec<u8>;
}
```
**Testy:** push 1000 poleceń → jedno `flush`; `flush_if_due` przed deadline → `None`; po deadline → `Some`; `force_flush` na pustym → pusta ramka lub `None` (zdecydować i przetestować); przekroczenie `MAX_BATCH` → auto-flush; polecenie z sync (`vkWaitForFences`) → natychmiastowy flush (test); kolejność zachowana (test na 100 poleceniach).
**Pułapki:** deadline liczony od **pierwszego** polecenia w batchu, nie od ostatniego (inaczej głodzenie); sync polecenia muszą wymuszać flush; rozmiar batcha ograniczony (pamięć).
**Ściągawka:** `std::time::Instant`, `gpu-proto::encode_command`.

---

### M9. `gpu-renderd` — odbiorca i wykonawca (`src/exec.rs`)
**Wejście/wyjście:**
```rust
pub struct Executor { /* real Vulkan device, HandleTable */ }
impl Executor {
    pub fn execute(&mut self, cmd: &VkCommand) -> Result<Vec<u8>, ExecError>;
    pub fn execute_batch(&mut self, batch: &[VkCommand]) -> Result<Vec<u8>, ExecError>;
}
```
**Testy:** wykonanie `vkCreateBuffer` z poprawnymi argumentami → uchwyt; z `size = 0` → `Err`; z nieznanym `wire_id` → `Err(UnknownHandle)`; batch z 100 poleceń — kolejność; batch z błędem w środku — stan po błędzie (zdecydować: rollback czy partial); polecenie z `pNext` cyklem → `Err`; **fuzz** 10 000 losowych bajtów jako `VkCommand` → brak paniki, tylko `Err`.
**Pułapki:** dane z sieci → **każdy** argument walidowany przed wywołaniem realnego Vulkan; `unsafe` przy `transmute` struktur — używać `bytemuck` (MIT/Apache) lub ręcznej walidacji; błąd Vulkan → mapować na `VkResult`, nie panikować.
**Ściągawka:** `ash` (MIT/Apache) — **podać pełne API**: `ash::Entry::load`, `ash::Instance::create`, `Device::create_buffer`, `vk::BufferCreateInfo`; `bytemuck`.

---

### M10. `gpu-video` — enkoder NVENC (`src/nvenc.rs`)
**Wejście/wyjście:**
```rust
pub struct NvencEncoder { /* dlopen'd libnvidia-encode */ }
impl NvencEncoder {
    pub fn new(cfg: &EncoderConfig) -> Result<Self, VideoError>;
    pub fn encode(&mut self, frame: &[u8], pts: u64) -> Result<Vec<u8>, VideoError>;
}
```
**Testy:** `new` bez libnvidia-encode → `Err(NotAvailable)` (nie crash); `encode` na pustej klatce → `Err`; `encode` na klatce złego rozmiaru → `Err`; 100 klatek → 100 pakietów; rozmiar pakietu < `MAX_PACKET`; `Drop` zwalnia sesję (test z `nvidia-smi` — brak wycieku sesji).
**Pułapki:** **licencja NVENC** — `dlopen` + własne deklaracje FFI, bez nagłówków w repo (patrz R9); `unsafe` przy FFI — każdy wskaźnik sprawdzany; NVENC wymaga CUDA context — zarządzać cyklem życia; błędy NVENC → mapować na `VideoError`, nie panikować.
**Ściągawka:** `libloading` (ISC — **sprawdzić, czy przechodzi!** jeśli nie, `libc::dlopen`), `libc`. Podać sygnatury: `NvEncodeAPICreateInstance`, `NV_ENCODE_API_FUNCTION_LIST`, `NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS`.

---

### M11. `gpu-video` — dekoder VA-API (`src/vaapi.rs`)
**Wejście/wyjście:**
```rust
pub struct VaapiDecoder { /* dlopen'd libva */ }
impl VaapiDecoder {
    pub fn new(cfg: &DecoderConfig) -> Result<Self, VideoError>;
    pub fn decode(&mut self, packet: &[u8]) -> Result<Frame, VideoError>;
}
```
**Testy:** `new` bez libva → `Err(NotAvailable)`; `decode` na uciętym pakiecie → `Err`; `decode` na losowych bajtach → `Err`, nie crash; 100 pakietów → 100 klatek; `Drop` zwalnia kontekst; rozmiar klatki zgodny z konfiguracją.
**Pułapki:** VA-API ma wiele wersji API — sprawdzać `vaInitialize` i wersję; `unsafe` FFI; pakiety z sieci → walidacja przed dekoderem (dekoder może mieć własne bugi — sandbox?); **licencja** libva = MIT, OK.
**Ściągawka:** `libloading` lub `libc::dlopen`, `libc`. Podać: `vaInitialize`, `vaCreateConfig`, `vaCreateContext`, `vaBeginPicture`, `vaRenderPicture`, `vaEndPicture`, `vaSyncSurface`.

---

### M12. `gpu-tools` — harness pomiarowy (`src/latency.rs`, `src/fuzz.rs`)
