# Konsultacja 07: M2b wirtualny swapchain (DeepSeek Flash na .2, 2026-11-02)

STATUS: odpowiedź modelu; przegląd Claude na końcu pliku.

---

# M2b: wirtualny swapchain i prezentacja

## 1. Rozszerzenia i podział odpowiedzialności

**Instancja (ICD ogłasza i implementuje):**
- `VK_KHR_surface` — obowiązkowo; bez niego DXVK nie wystartuje.
- `VK_KHR_wayland_surface` — priorytet (Omarchy/Hyprland). `VK_KHR_xcb_surface` + `VK_KHR_xlib_surface` — dla gier, które i tak wymuszają X11 (Proton często woli XWayland; DXVK sam wybiera surface z `SDL_Vulkan_*`). Ogłoś oba, ale Wayland zaimplementuj pierwszy.
- `VK_KHR_get_physical_device_properties2` — i tak potrzebne.
- `VK_EXT_swapchain_colorspace` — opcjonalnie, dopiero gdy HDR.

**Urządzenie:**
- `VK_KHR_swapchain` — obowiązkowo.
- `VK_KHR_swapchain_maintenance1` — **tak**, bo DXVK go używa (present fences, `VK_KHR_present_id`/`present_wait`). Bez niego DXVK działa, ale traci precyzyjne sterowanie tempem. Ogłoś dopiero, gdy zaimplementujesz `vkGetSwapchainImagesKHR` z `VkSwapchainPresentFenceInfoEXT` i `vkWaitForPresentKHR`.
- `VK_KHR_present_id` + `VK_KHR_present_wait` — dopiero w M6 (spekulacja/pacing). W M2b nie ogłaszaj.
- `VK_KHR_incremental_present` — nie teraz.

**Kto implementuje `vkCreateWaylandSurfaceKHR`?** ICD. Loader tylko przekazuje wskaźnik funkcji z `vk_icdGetInstanceProcAddr`. Surface to **nasz** obiekt klienta (uchwyt), który trzyma `wl_display*`/`wl_surface*` (lub `xcb_connection_t`+`xcb_window_t`). Serwer nie potrzebuje surface — obrazy swapchaina żyją na RTX jako zwykłe `VkImage` bez WSI.

**Zapytania o powierzchnię — pytamy lokalny Vulkan Intela, nie zmyślamy.** Powód: aplikacja porówna `surfaceCapabilities.currentExtent` z rozmiarem okna i format z tym, co potrafi wyświetlić. Jeśli odpowiemy „cokolwiek", DXVK wybierze format, którego Intel nie umie zaprezentować, i dostaniemy konwersję w najgorszym miejscu.

Realizacja: `gpu-icd` przy `vkCreateInstance` tworzy **drugą, lokalną instancję Vulkana** na Intelu (ładowaną przez `dlopen("libvulkan.so.1")` z jawnym `VK_ICD_FILENAMES` wskazującym intel_icd.json — nie przez nasz ICD). Ta instancja służy **wyłącznie** do WSI: surface, capabilities, formats, present modes, oraz docelowo do prezentacji. To nie jest obejście „bez opierania się na cudzych projektach" — to użycie sterownika systemowego jako warstwy wyświetlania, tak jak robi każdy kompozytor.

Odpowiedzi:
- `vkGetPhysicalDeviceSurfaceSupportKHR` → `VK_TRUE` dla naszego urządzenia (jeśli lokalny Intel umie daną powierzchnię; inaczej `VK_FALSE`).
- `vkGetPhysicalDeviceSurfaceCapabilitiesKHR` → od lokalnego Intela, ale nadpisz: `minImageCount = 3`, `maxImageCount = 3` (patrz §3), `supportedUsageFlags` z serwera (RTX 3090 Ti da `COLOR_ATTACHMENT|TRANSFER_SRC|SAMPLED`), `currentExtent` z lokalnego (Wayland: `0xFFFFFFFF` = „wybierz sam").
- `vkGetPhysicalDeviceSurfaceFormatsKHR` → **przecięcie** formatów lokalnego Intela i serwera; praktycznie `B8G8R8A8_UNORM` + `B8G8R8A8_SRGB` (DXVK tego chce). `R8G8B8A8_UNORM` dodaj jako drugi — patrz §6.
- `vkGetPhysicalDeviceSurfacePresentModesKHR` → `VK_PRESENT_MODE_FIFO_KHR` (obowiązkowy) + `VK_PRESENT_MODE_IMMEDIATE_KHR` jeśli Intel go ma. `MAILBOX` **nie ogłaszaj** w M2b — obiecuje semantykę, której nie dowieziemy do M6.

## 2. Wirtualny swapchain

**Obrazy:** `vkCreateSwapchainKHR` na kliencie wysyła żądanie do serwera (rozmiar, format, usage, liczba obrazów). Serwer tworzy N prawdziwych `VkImage` na RTX + N buforów readback (`VkBuffer`, `HOST_VISIBLE|HOST_COHERENT`, usage `TRANSFER_DST`). Klient dostaje N uchwytów `VkImage` (nasze, nie prawdziwe) i zapisuje je w tabeli mapowania. `vkGetSwapchainImagesKHR` zwraca te uchwyty — aplikacja robi na nich `vkCmdPipelineBarrier`, `vkCmdBlitImage` itd., a nasz koder poleceń tłumaczy uchwyt na zdalny przy replay.

**Semafory i fence — po stronie serwera, ale aplikacja czeka przez nasze polecenia.** Kluczowa decyzja: `vkAcquireNextImageKHR` **nie może** blokować na sieci. Realizacja:
- Klient trzyma lokalny licznik „ile obrazów jest wolnych" (odpowiedzi z serwera aktualizują ten licznik asynchronicznie).
- `vkAcquireNextImageKHR` zwraca natychmiast indeks z lokalnej puli wolnych obrazów. Jeśli semafor/fence podano: **sygnalizujemy je lokalnie** (nasz semafor to obiekt klienta; `vkWaitSemaphores`/`vkGetFenceStatus` obsługujemy sami, bez sieci). To jest trik 3 w czystej postaci: aplikacja „wie", że obraz jest gotowy, bo my tak twierdzimy — a prawdziwe bezpieczeństwo zapewnia to, że obraz wraca do puli dopiero po potwierdzeniu z serwera, że `vkQueuePresentKHR` się zakończył.
- Jeśli pula wolnych jest pusta: zwracamy `VK_NOT_READY` (jeśli aplikacja podała `timeout=0`) albo czekamy na **lokalnym** warunku (condvar), nie na TCP. Timeout z `vkAcquireNextImageKHR` mapujemy na oczekiwanie na condvar z deadline.
- `vkQueueSubmit` z semaforem acquire: nasz semafor jest lokalny, więc submit nie musi czekać na nic — wysyłamy polecenia do serwera, a serwer wykonuje je w kolejności. Kolejność gwarantuje uporządkowany strumień TCP.

**`vkQueuePresentKHR`:** wysyła do serwera żądanie `Present{swapchain, imageIndex, presentId}`. Serwer:
1. `vkCmdCopyImageToBuffer` z obrazu swapchaina do bufora readback (na kolejce transferu, z barierą).
2. Czeka na fence kopii.
3. Koduje (M2b: surowe RGBA8; M5: NVENC) i wysyła ramkę `Frame{swapchain, imageIndex, seq, payload}` **na osobnym strumieniu** (patrz §4).
4. Po wysłaniu zwalnia obraz do puli i wysyła `PresentDone{imageIndex, seq}` na strumieniu poleceń.

**Prezentacja lokalna:** osobna, lokalna instancja Vulkana na Intelu (ta z §1) tworzy **własny** `VkSwapchainKHR` na tym samym `wl_surface`. Klient dekoduje ramkę do `VkImage` lokalnego swapchaina (M2b: `vkCmdCopyBufferToImage` z bufora staging; M5: `vkCmdDecodeVideoKHR` lub `vkCmdCopyImage` z dekodera VA-API przez `VK_KHR_external_memory_fd`), potem `vkQueuePresentKHR` na lokalnej kolejce Intela. **Nigdy nie prezentujemy przez nasz ICD** — to byłaby pętla. Nasz ICD jest jedynym w `VK_DRIVER_FILES` dla gry; Intel ładowany jawnie przez `dlopen` z pominięciem loadera.

## 3. Synchronizacja i potok klatek

- **3 obrazy** (min=max=3). Mniej = aplikacja czeka; więcej = opóźnienie rośnie. DXVK domyślnie chce 3.
- **Nie blokujemy na klatce.** Aplikacja robi `Acquire` (natychmiast z puli), `Submit` (fire-and-forget), `Present` (fire-and-forget). Czeka tylko wtedy, gdy pula wolnych jest pusta — a to znaczy, że serwer nie nadąża, i wtedy **chcemy** backpressure.
- **`VK_SUBOPTIMAL_KHR`** zwracamy z `Acquire`/`Present`, gdy rozmiar okna różni się od `currentExtent` swapchaina. `VK_ERROR_OUT_OF_DATE_KHR` — gdy rozmiar się zmienił i aplikacja musi odtworzyć swapchain. Wayland: nasłuchujemy `xdg_surface.configure` w wątku WSI; aktualizujemy `currentExtent`; następne `Acquire` zwraca `OUT_OF_DATE`. Nie odtwarzamy swapchaina za aplikację.
- **Fence present (maintenance1):** `vkQueuePresentKHR` z `VkSwapchainPresentFenceInfoEXT` — fence sygnalizujemy lokalnie, gdy dostaniemy `PresentDone` z serwera. To jest jedyne miejsce, gdzie aplikacja realnie czeka na sieć, i tylko jeśli sama tego chce (DXVK używa do limitowania klatek).

## 4. Protokół

**Dwa połączenia TCP** (albo jedno TCP + jeden strumień w ramach tego samego połączenia z nagłówkiem typu ramki — prostsze w implementacji, bo jeden socket, ale wymaga multipleksera). Rekomendacja: **jedno połączenie TCP, ramki z typem**:
- `Cmd` (klient→serwer): partie poleceń, jak teraz.
- `Reply` (serwer→klient): odpowiedzi Query/Sync, `PresentDone`.
- `Frame` (serwer→klient): nagłówek `{swapchain, imageIndex, seq, w, h, format, codec, len}` + payload. Duże ramki (8 MB) — trzeba pilnować, żeby nie blokowały `Reply` (head-of-line blocking w TCP). Dlatego **frame idzie osobnym połączeniem TCP** — to jest właściwa decyzja. Dwa sockety: `control` (Cmd/Reply) i `video` (Frame). Oba na tym samym hoście, RTT 0,55 ms, więc narzut znikomy.
- **Backpressure:** serwer wysyła `Frame` tylko gdy klient potwierdził poprzednią (`FrameAck{seq}`) lub gdy okno (np. 2 ramki) na to pozwala. Klient wysyła `FrameAck` po zdekodowaniu i prezentacji. To naturalnie ogranicza produkcję klatek do tempa prezentacji.
- **Pomiar:** każda ramka ma `seq` i `t_present_submit` (timestamp serwera); klient loguje `t_frame_recv - t_present_submit` = opóźnienie klatki. Osobno mierzymy `t_acquire` (powinno być ~0) i `t_present_done`.

## 5. Rozmiary i pomiar

1080p RGBA8 = 8,3 MB/klatkę. Przy 1 Gb/s = ~120 MB/s → **~14 FPS surowo**. To potwierdza, że M2b jest tylko pomostem do M5. Mierzymy:
- **bajty/klatkę wg klasy** (Cmd, Reply, Frame) — osobne liczniki.
- **obiegi/klatkę** — ile razy aplikacja czekała na sieć (powinno być 0 w ścieżce gorącej).
- **czas klatki** — od `Present` do `FrameAck` (end-to-end), oraz od `Present` do `PresentDone` (bez wideo).
- **FPS** w vkcube i w grze.

**Trait `Encoder`/`Decoder` od razu:**
```rust
trait Encoder { fn encode(&mut self, img: &FrameView) -> Result<EncodedFrame>; }
trait Decoder { fn decode(&mut self, f: &EncodedFrame, dst: &mut FrameView) -> Result<()>; }
```
M2b: `RawRgbaEncoder`/`RawRgbaDecoder`. M5: `NvencEncoder`/`VaapiDecoder` — podmiana bez zmiany protokołu (pole `codec` w nagłówku `Frame`). To jest jedyny sposób, żeby M5 nie wymagał przepisania M2b.

## 6. Pułapki

- **Proton/DXVK:** DXVK wymaga `VK_KHR_swapchain` + `VK_KHR_surface` + surface extension. Sprawdza `VkPhysicalDeviceFeatures` i `VkPhysicalDeviceVulkan12Features` — nasz ICD musi je raportować zgodnie z RTX (przez migawkę z §3 planu). DXVK używa `VK_KHR_present_id`/`present_wait` jeśli są — nie ogłaszaj przed M6.
- **`VK_KHR_present_wait`:** jeśli ogłosisz, DXVK będzie czekał na `vkWaitForPresentKHR`. W M2b to zawiesi grę. Nie ogłaszaj.
- **HDR:** nie teraz. `VK_EXT_swapchain_colorspace` pomiń.
- **Format:** `B8G8R8A8_UNORM` to standard DXVK. `R8G8B8A8_UNORM` — Intel go umie, ale DXVK wybierze BGRA. **Serwer koduje w formacie, który wybrał klient** — nie konwertuj na serwerze, konwertuj w dekoderze (M5: NVENC chce NV12, więc konwersja i tak będzie; w M2b surowo BGRA).
- **Wayland rozmiar okna:** `xdg_surface.configure` przychodzi asynchronicznie; `currentExtent` musi być aktualizowane w wątku WSI, a `Acquire` czyta je atomowo. Bez tego dostaniesz `OUT_OF_DATE` w złym momencie.
- **Kursory i focus:** nie dotykamy — to robi kompozytor. Nasz ICD nie tworzy okien.
- **Najczęstsza pułapka zdalnego swapchaina:** aplikacja robi `vkDeviceWaitIdle` i oczekuje, że wszystko się skończyło — a nasze `PresentDone` jeszcze nie przyszło. `vkDeviceWaitIdle` musi czekać na wszystkie `PresentDone` (lokalny condvar), inaczej przy zamykaniu gry zgubimy ostatnią klatkę i dostaniemy wyciek pamięci na serwerze.

## 7. Podział na moduły (kolejność)

**Łatwe (mały model, gotowe testy):**
1. `gpu-wire::frame` — nagłówek `Frame`, serializacja, testy okrężne. (test: roundtrip losowych nagłówków)
2. `gpu-icd::wsi::surface` — rejestracja `wl_surface`/`xcb_window_t`, mapowanie uchwytów. (test: unit na mapie)
3. `gpu-icd::wsi::caps` — pytanie lokalnego Intela o capabilities/formats, przecięcie z serwerem. (test: mock lokalnego ICD)
4. `gpu-renderd::readback` — `vkCmdCopyImageToBuffer` + fence + odczyt. (test: integracyjny na RTX)
5. `gpu-icd::encoder::raw` — `RawRgbaEncoder`/`Decoder`. (test: roundtrip pikseli)

**Średnie (mały model + przegląd):**
6. `gpu-icd::wsi::swapchain` — `vkCreateSwapchainKHR`, `vkGetSwapchainImagesKHR`, pula wolnych obrazów, condvar. (test: symulacja acquire/present bez sieci)
7. `gpu-icd::wsi::present` — `vkQueuePresentKHR`, `PresentDone`, fence present. (test: integracyjny z mockiem serwera)
8. `gpu-renderd::frame_send` — wysyłka `Frame` na osobnym sockecie, backpressure, `FrameAck`. (test: loopback z opóźnieniem)
9. `gpu-icd::local_present` — lokalna instancja Intela, `dlopen`, własny swapchain, `vkQueuePresentKHR` na Intelu. (test: vkcube na Intelu bez naszego ICD)

**Trudne (tylko starszy inżynier, unsafe/synchronizacja):**
10. `gpu-icd::wsi::surface_wayland` — `unsafe` FFI do `libwayland-client`, `xdg_surface.configure`, wątek WSI, atomowe `currentExtent`. **Za trudne dla małego modelu.**
11. `gpu-icd::wsi::surface_xcb` — analogicznie, `unsafe` FFI do `libxcb`. **Za trudne.**
12. `gpu-icd::local_present::sync` — synchronizacja między wątkiem dekodera, wątkiem WSI i wątkiem poleceń; `vkDeviceWaitIdle` czekający na `PresentDone`. **Za trudne — tu jest całe ryzyko zakleszczenia.**

Kolejność: 1→2→3→4→5 (fundament), 6→7→8 (swapchain bez prezentacji), 9 (prezentacja lokalna), 10/11 (surface), 12 (synchronizacja). Kamień milowy M2b: vkcube rysuje na RTX, klatka wraca surowo, wyświetla się na Intelu, `Acquire` nie blokuje na sieci.
---

## Przegląd (Claude, 2026-11-02)

**Przyjęte:** obrazy swapchaina żyją na serwerze, prezentacja lokalnym swapchainem Intela (nigdy przez nasz ICD), 3 obrazy, `Acquire` bez czekania na sieć, backpressure przez `FrameAck`, osobne połączenie na wideo (head-of-line blocking dużych ramek), trait `Encoder`/`Decoder` z polem `codec`, nie ogłaszać `present_wait`/HDR/`MAILBOX` przed M6, `vkDeviceWaitIdle` czekający na `PresentDone`.

**Błędy w odpowiedzi:**
1. **Semafor acquire "sygnalizowany lokalnie" zawiesiłby GPU.** Semafor podany do `vkAcquireNextImageKHR` trafia potem do `VkSubmitInfo.pWaitSemaphores`, a `vkQueueSubmit` jest wykonywany na serwerze na PRAWDZIWYM semaforze, którego nikt nie zasygnalizował. Poprawka: `Acquire` to polecenie asynchroniczne w strumieniu (bez odpowiedzi), po którym serwer robi pusty `vkQueueSubmit` sygnalizujący semafor/fence aplikacji. Indeks obrazu nadaje klient z lokalnej puli, więc aplikacja nadal nie czeka na sieć.
2. **"Druga instancja Vulkana na Intelu przez `dlopen` z `VK_ICD_FILENAMES`" nie da osobnego loadera.** `dlopen` tej samej biblioteki zwraca ten sam załadowany loader, a zmienne środowiskowe czyta loader przy `vkCreateInstance` (zmiana w locie jest wyścigiem). Wybieram: **załadować sterownik Intela bezpośrednio** (`dlopen` jego ICD, np. `libvulkan_intel.so`, i `vk_icdGetInstanceProcAddr`), z pominięciem loadera. Powierzchnię Wayland przekazuje wtedy `VkIcdSurfaceWayland{platform, wl_display*, wl_surface*}`, którą nasz ICD dostaje od loadera jako uchwyt `VkSurfaceKHR` (jeśli ICD nie implementuje `vkCreateWaylandSurfaceKHR`, loader sam przekazuje własną strukturę z wskaźnikami). Wariant zapasowy: dołożyć ICD Intela do `VK_DRIVER_FILES` i tworzyć lokalną instancję przez loader.
3. Drobne: `present fence` w `VkSwapchainPresentFenceInfoEXT` idzie w `pNext` `vkQueuePresentKHR`, nie w `vkGetSwapchainImagesKHR`. Serwer musi tworzyć obrazy swapchaina z dodatkowym `TRANSFER_SRC` i czekać w kopii readback na semafory z `VkPresentInfoKHR`.

**Kolejność prac M2b (zmieniona):**
1. Walidacja ścieżki graficznej OFFSCREEN w `gpu-smoke` (obraz, bariery, `vkCmdClearColorImage`, `vkCmdCopyImageToBuffer`, odczyt do PPM), bo struktury i bariery to najbardziej typowa przyczyna błędów kodeka; dopiero potem WSI.
2. Rozszerzenia powierzchni po stronie ICD (zapytania o capabilities/formats z lokalnego Intela).
3. Wirtualny swapchain + `Present` + ramka `Frame` na drugim gnieździe (surowo), potem lokalna prezentacja.
4. `vkcube`.
