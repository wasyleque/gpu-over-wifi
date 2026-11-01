# Konsultacja 05: VulkanBackend (DeepSeek Flash na .2, 2026-11-01)

Odpowiedź modelu poniżej bez zmian, a przegląd (Claude) na końcu.

---

# VulkanBackend dla gpu-renderd — projekt

Poniżej konkret: ładowanie, uchwyty, łańcuchy pNext, kodowanie danych wyjściowych, testy, pułapki, podział na moduły.

## 1. Ładowanie libvulkan.so.1 bez `ash`

**Wybór: `libc` (MIT/Apache-2.0) + własne `extern "C"` na `dlopen/dlsym/dlclose`.** Nie linkujemy `-ldl` ręcznie: od glibc 2.34 `libdl` jest scalona z `libc`, a `libc` crate i tak linkuje `libc.so.6`, w której `dlopen`/`dlsym` są eksportowane. Na glibc 2.39 (Arch, Ubuntu 24.04) `-ldl` to puste ogniwo — niepotrzebne. Nie używamy `libloading` (choć jest MIT) — mniej zależności, pełna kontrola nad `RTLD_LOCAL` i obsługą błędów.

```rust
// gpu-renderd/src/vk/loader.rs
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

const RTLD_NOW: c_int = 2;
const RTLD_LOCAL: c_int = 0;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *mut c_char;
}

pub struct LibVulkan {
    handle: *mut c_void,
    pub gipa: PFN_vkGetInstanceProcAddr,
}

type PFN_vkGetInstanceProcAddr =
    unsafe extern "C" fn(instance: VkInstance, name: *const c_char) -> PFN_vkVoidFunction;
type PFN_vkVoidFunction = unsafe extern "C" fn();

impl LibVulkan {
    pub fn load() -> Result<Self, LoadError> {
        // Kolejność: env override (testy), potem standardowe nazwy.
        let candidates = [
            std::env::var("GPU_RENDERD_VULKAN_LIB").ok(),
            Some("libvulkan.so.1".into()),
            Some("libvulkan.so".into()),
        ];
        for cand in candidates.into_iter().flatten() {
            let c = CString::new(cand.clone()).unwrap();
            let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
            if h.is_null() { continue; }
            let sym = unsafe { dlsym(h, c"vkGetInstanceProcAddr".as_ptr()) };
            if sym.is_null() {
                unsafe { dlclose(h); }
                continue;
            }
            let gipa: PFN_vkGetInstanceProcAddr = unsafe { std::mem::transmute(sym) };
            return Ok(Self { handle: h, gipa });
        }
        Err(LoadError::NotFound(dlerror_string()))
    }
}
impl Drop for LibVulkan {
    fn drop(&mut self) { if !self.handle.is_null() { unsafe { dlclose(self.handle); } } }
}
```

**Tabela wskaźników — M1 (~12 funkcji).** Nie ładujemy globalnie; ładujemy przez `vkGetInstanceProcAddr(instance, name)` po utworzeniu instancji (dla funkcji instancyjnych) i `gipa(null, name)` dla `vkCreateInstance`/`vkEnumerateInstance*`. To zgodne ze specyfikacją i pozwala obsłużyć wiele instancji.

```rust
pub struct InstanceFns {
    pub destroy_instance: PFN_vkDestroyInstance,
    pub enumerate_physical_devices: PFN_vkEnumeratePhysicalDevices,
    pub get_physical_device_properties2: PFN_vkGetPhysicalDeviceProperties2,
    pub get_physical_device_features2: PFN_vkGetPhysicalDeviceFeatures2,
    pub get_physical_device_memory_properties2: PFN_vkGetPhysicalDeviceMemoryProperties2,
    pub get_physical_device_format_properties2: PFN_vkGetPhysicalDeviceFormatProperties2,
    pub get_physical_device_queue_family_properties2: PFN_vkGetPhysicalDeviceQueueFamilyProperties2,
    pub get_physical_device_extensions: PFN_vkEnumerateDeviceExtensionProperties,
    pub create_device: PFN_vkCreateDevice,
    pub get_device_proc_addr: PFN_vkGetDeviceProcAddr,
    pub destroy_device: PFN_vkDestroyDevice,
    pub enumerate_instance_extension_properties: PFN_vkEnumerateInstanceExtensionProperties,
}
```

Ładowanie: makro `load!(gipa, instance, name)` z `transmute` po sprawdzeniu `null`. Brak symbolu = `Err(MissingSymbol(name))`, a nie panika — serwer ma raportować klientowi `VK_ERROR_INITIALIZATION_FAILED`.

**Uwaga:** `vkGetInstanceProcAddr` zwraca `NULL` dla funkcji, których instancja nie wspiera (np. `vkGetPhysicalDeviceProperties2` bez `VK_KHR_get_physical_device_properties2` na starym loaderze). Dlatego M1 wymaga, by na serwerze był loader ≥ 1.1 (Vulkan 1.1 promuje te funkcje do core). Sprawdzamy `vkEnumerateInstanceVersion` i odrzucamy loader < 1.1.

## 2. Mapowanie u64 ↔ uchwyty dyspozycyjne

Uchwyty dyspozycyjne (`VkInstance`, `VkPhysicalDevice`, `VkDevice`, `VkQueue`, `VkCommandBuffer`) to wskaźniki — nie wolno ich wysyłać do klienta. Uchwyty niedyspozycyjne (`VkBuffer`, `VkImage`, …) też trzymamy w tablicy, bo klient nie może ich zgadywać.

**Slot table z generacją** (32-bit slot + 32-bit generacja = u64):

```rust
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,   // free-list przez next_free
    free_head: u32,
    generation: u32,       // globalna, rośnie przy każdym zwolnieniu
}
struct Slot<T> { gen: u32, val: Option<T>, next_free: u32 }

pub struct Handle(u64);
impl Handle {
    fn new(slot: u32, gen: u32) -> Self { Handle(((gen as u64) << 32) | slot as u64) }
    fn slot(self) -> u32 { self.0 as u32 }
    fn gen(self)  -> u32 { (self.0 >> 32) as u32 }
}

impl<T> HandleTable<T> {
    pub fn insert(&mut self, v: T) -> Handle {
        let slot = if self.free_head != u32::MAX {
            let s = self.free_head; self.free_head = self.slots[s as usize].next_free; s
        } else { self.slots.push(Slot{gen:0,val:None,next_free:u32::MAX}); (self.slots.len()-1) as u32 };
        self.generation = self.generation.wrapping_add(1);
        let g = self.generation;
        self.slots[slot as usize] = Slot{ gen: g, val: Some(v), next_free: u32::MAX };
        Handle::new(slot, g)
    }
    pub fn get(&self, h: Handle) -> Option<&T> {
        let s = self.slots.get(h.slot() as usize)?;
        if s.gen == h.gen() { s.val.as_ref() } else { None }
    }
    pub fn remove(&mut self, h: Handle) -> Option<T> {
        let s = self.slots.get_mut(h.slot() as usize)?;
        if s.gen != h.gen() { return None; }
        let v = s.val.take();
        s.next_free = self.free_head; self.free_head = h.slot();
        v
    }
}
```

**Zasady bezpieczeństwa wobec wrogiego klienta:**
- Każdy uchwyt z sieci przechodzi przez `get()`; `None` → `VK_ERROR_INVALID_OPAQUE_CAPTURE_ADDRESS`-podobny błąd protokołu (albo `VK_ERROR_DEVICE_LOST` jeśli klient uparcie spamuje — patrz §6 planu).
- Uchwyty są **per sesja** (sesja z uwierzytelnianiem już istnieje) — klient A nie może użyć uchwytu klienta B. Tablica żyje w `Session`.
- `VkPhysicalDevice` mapujemy osobno: to uchwyt dyspozycyjny, ale nie tworzymy go — dostajemy z `vkEnumeratePhysicalDevices`. Wstawiamy do tablicy przy enumeracji i **unieważniamy przy `destroy_instance`** (cała tablica fizycznych urządzeń danej instancji).
- `VkDevice` i `VkQueue` — osobne tablice, `VkQueue` powiązany z `VkDevice` (walidacja: kolejka należy do urządzenia z tej samej sesji).
- Generacja globalna rośnie monotonicznie — po 2^32 zwolnieniach zawija się; to akceptowalne (ryzyko kolizji z starym uchwytem po 4 mld operacji; można podnieść do 48-bit slotu, ale 32/32 wystarcza dla M1–M7).

**Dlaczego nie surowe wskaźniki:** klient mógłby wysłać `0xdeadbeef` i wywołać `vkDestroyInstance` na śmieciu → segfault serwera. Slot table to eliminuje.

## 3. `get_physical_device` z łańcuchem pNext

Protokół: `PdFunc` (enum), `arg: u32` (np. format, queue family index), `count: u32`, `chain: &[u32]` (lista sType). Serwer buduje łańcuch w pamięci, wywołuje `vkGetPhysicalDevice*2`, zwraca surowe bajty.

**Budowa łańcucha.** `gpu-proto::stype::lookup(sType) -> Option<&'static StypeInfo { name, size, align }>`. Alokujemy bufor z **wyrównaniem maksymalnym** (`align_of::<u64>()` wystarcza dla wszystkich struktur Vulkan — sprawdzić w generatorze, czy nie ma `align > 8`; na x86_64 Vulkan wymaga max 8). Każda struktura na osobnym offsetcie wyrównanym do `align`, `pNext` ustawiamy na następną.

```rust
pub struct ChainBuf { buf: Vec<u8>, offsets: Vec<usize> }

fn build_chain(chain: &[u32]) -> Result<ChainBuf, ChainError> {
    if chain.len() > 64 { return Err(ChainError::TooLong); }
    let mut seen = std::collections::HashSet::new();
    let mut offsets = Vec::with_capacity(chain.len());
    let mut total = 0usize;
    for &st in chain {
        if !seen.insert(st) { return Err(ChainError::Duplicate(st)); }
        let info = stype::lookup(st).ok_or(ChainError::Unknown(st))?;
        if info.has_output_pointers {
            return Err(ChainError::UnsupportedOutputPointers(st));
        }
        total = align_up(total, info.align.max(8));
        offsets.push(total);
        total += info.size;
    }
    let mut buf = vec![0u8; total.max(8)];
    for (i, &st) in chain.iter().enumerate() {
        let info = stype::lookup(st).unwrap();
        let off = offsets[i];
        // sType
        unsafe { ptr::write_unaligned(buf.as_mut_ptr().add(off) as *mut u32, st); }
        // pNext
        let next = if i + 1 < chain.len() { offsets[i+1] as *mut c_void } else { ptr::null_mut() };
        unsafe { ptr::write_unaligned(buf.as_mut_ptr().add(off + 8) as *mut *mut c_void, next); }
    }
    Ok(ChainBuf { buf, offsets })
}
```

**Wywołanie i zwrot.** Dla `VkPhysicalDeviceProperties2`:

```rust
unsafe {
    let mut head = VkPhysicalDeviceProperties2 { s_type: STYPE_PHYSICAL_DEVICE_PROPERTIES_2, p_next: chain_head, properties: zeroed() };
    fns.get_physical_device_properties2(pdev, &mut head);
}
```

Po wywołaniu **zerujemy wszystkie `pNext`** w zwróconym buforze (klient je przepisze na własne struktury — nie może dostać wskaźników serwera). Zwracamy `Vec<u8>` = cały bufor (head + chain) albo tylko chain — protokół już to definiuje; zakładam, że `get_physical_device` zwraca **cały blok** (nagłówek + chain), bo klient musi znać wartości z `VkPhysicalDeviceProperties2.properties`.

**Struktury ze wskaźnikami w wyjściu.** `VkPhysicalDeviceVulkan12Properties` jest bezpieczne (same skalary i tablice `char[VK_MAX_*]`). Problem to struktury, które zwracają **wskaźniki do pamięci hosta** — np. `VkPhysicalDeviceToolProperties` ma `char name[...]` (OK), ale `VkPhysicalDevicePCIBusInfoPropertiesEXT` jest OK, a `VkPhysicalDeviceLayeredApiPropertiesListKHR` (jeśli kiedyś) ma `pLayeredApis` — wskaźnik. Rozpoznanie: generator `gpu-vkgen` musi dla każdej struktury oznaczyć `has_output_pointers = true`, jeśli którykolwiek członek to `pointer` **bez** `const` i struktura jest w rodzinie `*Properties*`/`*Features*`/`*Limits*` (wyjściowa). Reguła: w łańcuchu `get_physical_device` **odrzucamy** każdą strukturę z nie-const wskaźnikiem — klient dostanie `ChainError::UnsupportedOutputPointers`. Wyjątki (np. `VkPhysicalDeviceExternalMemoryHostPropertiesEXT` — brak wskaźników) przechodzą. To bezpieczne i wystarczy dla M1–M7; rozszerzymy, gdy pojawi się realna potrzeba.

**Zasady wobec wrogiego klienta:**
- nieznany sType → `VK_ERROR_FEATURE_NOT_PRESENT` (nie panika);
- duplikat → j.w.;
- > 64 → j.w.;
- `arg` poza zakresem (np. format) → walidacja przed wywołaniem;
- `count` > rozsądnego limitu (np. 4096 dla queue families) → odrzucić;
- **nigdy** nie przekazujemy do Vulkan wskaźników z sieci — tylko własne bufory.

## 4. Dane o zmiennej długości (count + tablica)

`VkQueueFamilyProperties2` i `VkPhysicalDeviceMemoryProperties2` mają tablice w środku (`VkQueueFamilyProperties*` — tablica struktur; `VkMemoryHeap/Type` — tablice w `VkPhysicalDeviceMemoryProperties`). `VkPhysicalDeviceMemoryProperties2` jest **inline** (tablice stałej długości `VK_MAX_MEMORY_HEAPS/TYPES`), więc mieści się w `size` struktury — bez problemu.

`VkQueueFamilyProperties2` to tablica struktur — protokół musi ją obsłużyć osobno. Proponuję **dwa wywołania** (zgodnie z Vulkanem):
1. `get_physical_device(pdev, QueueFamilyProperties2, arg=0, count=0, chain)` → zwraca `u32` count (4 bajty) — albo `Data::Count(u32)`;
2. `get_physical_device(pdev, QueueFamilyProperties2, arg=0, count=N, chain)` → zwraca `N * size_of::<VkQueueFamilyProperties2>()` bajtów.

`Data` w protokole: `enum Data { Bytes(Vec<u8>), Count(u32), Error(VkResult) }`. Dla `VkPhysicalDeviceProperties2`/`Features2`/`MemoryProperties2`/`FormatProperties2` — `Bytes` (stały rozmiar). Dla `QueueFamilyProperties2` — `Count` przy `count==0`, `Bytes` przy `count>0`. Klient robi dwa rund-tripy przy pierwszym zapytaniu, ale to **raz na sesję** (migawka, trik 3) — nie w gorącej ścieżce.

**Uwaga:** `VkPhysicalDeviceFormatProperties2` ma `arg = VkFormat`; `VkPhysicalDeviceImageFormatInfo2` (jeśli kiedyś) — `arg` to indeks do tablicy struktur przekazanych w `chain` jako surowe bajty. Na M1 nie potrzebne.

## 5. Plan testów

**Bez GPU (CI, każdy PR):**
- **Atrapa libvulkan** — własna `cdylib` `libvulkan_stub.so` eksportująca `vkGetInstanceProcAddr` i ~12 funkcji, które zwracają z góry ustalone dane (np. `VkPhysicalDeviceProperties2` z `deviceName="Stub GPU"`, 2 queue families). Ładowana przez `GPU_RENDERD_VULKAN_LIB=...`. Testy: budowa łańcucha, zerowanie pNext, mapowanie uchwytów, odrzucanie złych sType.
- **Testy jednostkowe `HandleTable`**: insert/get/remove, generacja, fałszywy uchwyt, podwójne remove.
- **Testy `build_chain`**: duplikat, nieznany, > 64, struktura z output pointerem.
- **Testy okrężne kodowania** (już są w `gpu-wire`) — rozszerzyć o `Data`.

**Wymaga RTX 3090 Ti (testy integracyjne, `#[ignore]` + `cargo test -- --ignored`):**
- **`vulkaninfo`-podobny**: uruchamiamy `gpu-renderd` na serwerze, klient `gpu-icd` na laptopie, `vulkaninfo` przez ICD — sprawdzamy, że widzi `NVIDIA GeForce RTX 3090 Ti`, `driverName` nasz, i że **liczba rund-tripów przy starcie = 0** (migawka). To kryterium M1.
- **Porównanie z lokalnym `vulkaninfo`**: te same pola (deviceName, apiVersion, driverVersion, limits, memory heaps) — diff musi być pusty (poza `driverName`/`driverInfo`).
- **Test obciążeniowy**: 10 000 wywołań `get_physical_device` z różnymi łańcuchami — brak wycieków, czas < X ms.

**Testy ABI:** `static_assert`-y w Rust (`const _: () = assert!(size_of::<VkPhysicalDeviceProperties2>() == ...)`) generowane z `vk.xml` — łapią rozjazd z loaderem.

## 6. Pułapki

1. **ABI x86_64:** `VkInstance` itd. to `*mut c_void` (8 B); `VkBool32` = `u32` (nie `bool`!); `VkFlags` = `u32`; `VkDeviceSize` = `u64`. Wszystkie struktury `#[repr(C)]`. `size_of`/`align_of` generowane z `vk.xml` i sprawdzane testem.
2. **Wyrównanie:** `pNext` musi być wyrównany do `align_of::<*mut c_void>()` = 8. Bufor `Vec<u8>` ma `align=1` — **nie wolno** rzutować `&buf[off]` na `*mut VkStruct` bez sprawdzenia; używamy `ptr::write_unaligned`/`read_unaligned` albo `Vec<u64>` jako backing store. Rekomendacja: `Vec<u64>` (8 B align) i indeksowanie bajtowe.
3. **`VK_KHR_portability`:** na Linuksie nie dotyczy (to macOS/iOS). Ale `VK_KHR_portability_enumeration` może się pojawić w `vkEnumerateInstanceExtensionProperties` — przepuszczamy bez zmian.
4. **Instancje bez rozszerzeń:** `vkCreateInstance` z `apiVersion=1.0` a `vkGetPhysicalDeviceProperties2` — wymaga `VK_KHR_get_physical_device_properties2` lub Vulkan 1.1. Sprawdzamy `apiVersion` instancji i ładujemy `*2` tylko gdy dostępne; inaczej fallback na `vkGetPhysicalDeviceProperties` (bez chain) — ale M1 wymaga 1.1, więc odrzucamy 1.0.
5. **Validation layers w środowisku serwera:** `VK_LAYER_PATH`/`VK_INSTANCE_LAYERS` mogą być ustawione w środowisku demona — **czyścimy** je przed `vkCreateInstance` (albo jawnie przekazujemy `ppEnabledLayerNames=null`), żeby klient nie wymusił walidacji na serwerze. To też wektor ataku (klient wysyła nazwę warstwy → serwer ładuje dowolną `.so` z dysku). **Zasada: serwer ignoruje warstwy i rozszerzenia żądane przez klienta poza whitelistą.**
6. **`vkEnumerateInstanceVersion`:** ładujemy przez `gipa(null, ...)`; jeśli `NULL` → loader 1.0, odrzucamy.
7. **Wielowątkowość:** `vkGetInstanceProcAddr` jest thread-safe, ale nasze tablice uchwytów — nie. `Session` trzyma `Mutex<HandleTable>`; wywołania Vulkan do tego samego `VkDevice` są thread-safe wg specyfikacji (poza `vkQueueSubmit` na tej samej kolejce — serializujemy per kolejka).
8. **`dlclose` w `Drop`:** jeśli w tle działają wątki Vulkan — nie zamykamy biblioteki przed `vkDestroyInstance`/`vkDestroyDevice`. Kolejność: najpierw niszczymy instancje, potem `LibVulkan`.
9. **`VkPhysicalDevice` ważność:** po `vkDestroyInstance` wskaźnik fizycznego urządzenia jest nieważny — unieważniamy wpisy w tablicy.
10. **`VK_EXT_debug_utils`:** nie ładujemy na serwerze (brak sensu, a klient nie może wymusić).

## 7. Podział na moduły (kolejność dla małego modelu)

Każdy moduł: kod + testy jednostkowe (atrapa), PR < 400 linii.

1. **`vk/loader.rs`** — `LibVulkan::load`, `InstanceFns::load`, makro `load!`. Test: atrapa `libvulkan_stub.so` (osobny crate `gpu-vk-stub`), sprawdzenie braku symbolu.
2. **`vk/handles.rs`** — `HandleTable<T>`, `Handle`. Testy: insert/get/remove, generacja, fałszywy uchwyt.
3. **`vk/chain.rs`** — `build_chain`, `zero_pnext`, `ChainError`. Testy: duplikat, nieznany, > 64, output pointer.
4. **`vk/stype_meta.rs`** — rozszerzenie `gpu-vkgen` o `has_output_pointers` i `align`; test: dla wszystkich 1755 struktur `size_of`/`align_of` zgodne z `vk.xml`.
5. **`backend/mod.rs`** — `VulkanBackend` implementujący `GpuBackend` (7 metod), z `Session` i tablicami. Test: atrapa zwraca ustalone dane, porównanie bajtów.
6. **`backend/physical.rs`** — `get_physical_device` dla 5 funkcji `*2` + `QueueFamilyProperties2` (count/bytes). Test: atrapa + łańcuchy.
7. **`backend/instance.rs`** — `create_instance`/`destroy_instance`, whitelist rozszerzeń, czyszczenie env. Test: atrapa, odrzucenie złej warstwy.
8. **`backend/device.rs`** — `create_device`/`destroy_device`, `VkQueue` mapowanie. Test: atrapa.
9. **`tests/integration_vulkaninfo.rs`** — `#[ignore]`, wymaga RTX 3090 Ti; uruchamia `vulkaninfo` przez `gpu-icd` i porównuje z lokalnym.

Kolejność: 1→2→3→4 (fundament), 5→6 (M1), 7→8 (M1 domknięcie), 9 (walidacja na sprzęcie). Moduły 1–4 są w pełni testowalne bez GPU — to najlepszy start dla małego modelu.
---

## Przegląd (Claude)

Przyjęte: ładowanie przez własne `extern "C"` dlopen/dlsym; tabela uchwytów z generacją (uchwyty dyspozycyjne to adresy serwera, klient nie może ich zobaczyć: wyciek ASLR i segfault przy sfałszowanym uchwycie); czyszczenie `VK_LAYER_*` z env i ignorowanie warstw żądanych przez klienta; `Vec<u64>` jako bufor łańcucha (wyrównanie 8); odrzucanie struktur ze wskaźnikami wyjściowymi; kolejność modułów 1→9.

**Poprawki względem odpowiedzi:**
1. `build_chain` wpisywał do `pNext` *offsety* (`offsets[i+1] as *mut c_void`). Vulkan potrzebuje prawdziwych adresów (`base_ptr + offset`). Bufor `Vec<u64>` ma stabilny adres na stercie, więc wystarczy policzyć adres przy budowie.
2. Reguła `has_output_pointers`: odpowiedź ograniczała ją do rodzin `*Properties*` z nie-const wskaźnikiem. Prościej i bezpieczniej: **każdy** członek ze wskaźnikiem poza `pNext` (kontrola przez `ptr_depth > 0`), bez względu na nazwę.
3. Protokół: `Data::Count` nie jest potrzebny, bo `Response::Data` niesie 4 bajty LE z liczbą przy `count == 0` (zachowujemy istniejący kodek M1).
4. `libc` crate niepotrzebny, ale `dlopen` wymaga `-ldl` tylko na glibc < 2.34. Cel: 2.39, więc bez dodatkowych flag.
5. Odrzucenie sType: `ERR_UNKNOWN` z handlera już zwraca błąd bez paniki; kody Vulkana z odpowiedzi (`FEATURE_NOT_PRESENT`) mapujemy w backendzie.
