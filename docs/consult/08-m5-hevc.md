# Konsultacja 08: M5 HEVC (DeepSeek Flash na .2, 2026-11-02)

STATUS: odpowiedź modelu; przegląd Claude na końcu pliku.

---

# M5: HEVC encode (serwer) + decode (klient) — projekt

## 1. Serwer: trzy drogi

### (a) NVENC przez libnvidia-encode.so.1 (dlopen + własne FFI)

**Dostarczenie klatki do NVENC.** NVENC przyjmuje powierzchnie CUDA (najprostsze), D3D11, OpenGL lub — od niedawna — Vulkan (przez `NV_ENC_INPUT_RESOURCE_TYPE` z uchwytem Vulkan; w praktyce nieużywane i słabo udokumentowane). Realistyczne opcje:

1. **Vulkan → CUDA (zero-copy, docelowo).** Serwer renderuje do `VkImage` z `VK_KHR_external_memory_fd` (export dmabuf, `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT` na Linuksie). Następnie `cuImportExternalMemory` (CUDA Driver API, `cuMemGetHandleForAddressRange`/`cuImportExternalMemory` z `CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD`) → `cuExternalMemoryGetMappedMipmappedArray` lub `cuExternalMemoryGetMappedBuffer`. Z tego robimy `CUarray` (dla NV12 trzeba dwie płaszczyzny: Y i UV — albo jeden bufor z offsetami, albo dwie `CUarray`). Potem `nvEncRegisterResource` z `NV_ENC_REGISTER_RESOURCE` typu `NV_ENC_INPUT_RESOURCE_TYPE_CUDAARRAY`. **Pułapka:** NVENC oczekuje NV12, a my mamy BGRA. Konwersja BGRA→NV12 musi się stać: albo shaderem Vulkan do drugiego `VkImage` w formacie `VK_FORMAT_G8_B8R8_2PLANE_420_UNORM` (export dmabuf, import CUDA), albo `cuMemcpy2D` + własny kernel CUDA (wtedy trzeba `libcuda.so` i pisać PTX — nie chcemy). **Rekomendacja: shader Vulkan BGRA→NV12 do obrazu dwupłaszczyznowego, eksport dmabuf, import CUDA.** To jest realne i mieści się w naszym „własny kod".
2. **Kopia przez hosta (pierwszy krok, M5a).** `vkCmdCopyImageToBuffer` do `HOST_CACHED` (już mamy tę ścieżkę dla Raw!), potem `cuMemcpyHtoD` do `CUdeviceptr` zaalokowanego `cuMemAlloc`, konwersja BGRA→NV12 na CPU (prosta pętla, ~2 ms dla 1080p) lub `cuMemcpy2D` z pitch. Rejestracja jako `NV_ENC_INPUT_RESOURCE_TYPE_CUDEVICEPTR`. **To jest ścieżka na pierwszy kamień milowy** — działa bez interopu, akceptowalna na start.
3. **GL interop** — odrzucamy, nie mamy kontekstu GL.

**Wywołania (kolejność):**
```
nvEncOpenEncodeSessionEx(&{version, deviceType=CUDA, device=cuContext}, &session)
nvEncGetEncodeGUIDCount / nvEncGetEncodeGUIDs  // wybrać NV_ENC_CODEC_HEVC_GUID
nvEncGetEncodePresetCount/GUIDs, nvEncGetEncodePresetConfigEx
nvEncInitializeEncoder(session, &NV_ENC_INITIALIZE_PARAMS{
   encodeGUID=HEVC, presetGUID=P1 (low latency),
   encodeWidth, encodeHeight, frameRateNum/Den,
   enablePTD=1,   // picture decision
   bufferFormat=NV_ENC_BUFFER_FORMAT_NV12,
   tuningInfo=NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
   rcParams={rateControlMode=NV_ENC_PARAMS_RC_CBR (lub VBR),
             averageBitRate=40..100 Mbps, maxBitRate=...,
             vbvBufferSize=..., vbvInitialDelay=...,
             enableIntraRefresh=1, intraRefreshPeriod=...,
             enableLookahead=0, zerolatency=1},
   maxEncodeWidth/Height,
   // brak B-ramek: NV_ENC_HEVC_CONFIG maxNumRefFrames, numTemporalLayers=1
})
nvEncCreateBitstreamBuffer(&bitstream)
nvEncRegisterResource(&NV_ENC_REGISTER_RESOURCE{resourceType=CUDAARRAY|CUDEVICEPTR,
   width, height, pitch, bufferFormat=NV12, bufferUsage=NV_ENC_INPUT_IMAGE})
// per frame:
nvEncMapInputResource(&map)
nvEncEncodePicture(&NV_ENC_PIC_PARAMS{
   inputBuffer=map.registeredResource, bufferFmt=NV12,
   pictureType=NV_ENC_PIC_TYPE_I (na żądanie IDR) | NV_ENC_PIC_TYPE_P,
   encodePicFlags=NV_ENC_PIC_FLAG_FORCEIDR (na żądanie),
   inputTimeStamp, outputBitstream=bitstream, completionEvent=...})
nvEncLockBitstream(&lock) → skopiuj NAL-e → nvEncUnlockBitstream
```
**Pułapki:**
- **Wersja API.** `NVENCAPI_MAJOR_VERSION`/`MINOR` muszą być ≤ wersji sterownika. Sprawdzaj `nvEncGetEncodeCaps` i `NV_ENC_CAPS_*`; przy niezgodności `nvEncInitializeEncoder` zwraca `NV_ENC_ERR_INCOMPATIBLE_VERSION`. Trzymaj się wersji z nagłówków SDK, którą sterownik 3090 Ti (Ampere, NVENC 7. gen) obsługuje — Ampere ma HEVC encode, ale **nie ma AV1 encode** (to Ada). HEVC Main/Main10 OK.
- **Licencja NVIDIA Video Codec SDK.** Nagłówki `nvEncodeAPI.h` mają własną licencję (NVIDIA Software License), **nie MIT/Apache/BSD**. Nie wolno ich vendorować do repo ani linkować statycznie w sposób sprzeczny z licencją. **Rozwiązanie:** piszemy własne deklaracje FFI (`#[repr(C)]` struktury, stałe) **na podstawie publicznej dokumentacji API** (NVIDIA Video Codec SDK API reference jest publiczna), nie kopiujemy nagłówków. To jest zgodne z „własny kod". Uwaga: to nie jest porada prawna — przed publikacją warto skonsultować; w kodzie zostawiamy komentarz o pochodzeniu deklaracji. Unikamy też linkowania `libnvidia-encode.so.1` w czasie linkowania — tylko `dlopen` + `dlsym` (jak już robimy z `libvulkan`).
- **Synchronizacja.** NVENC działa asynchronicznie; używamy `NV_ENC_EVENT_PARAMS` (event) lub `nvEncEncodePicture` z `completionEvent` i czekamy na fence Vulkan→CUDA (`cuStreamWaitValue`/`cuEventSynchronize` po `vkQueueSubmit` z semaforem). Na start: `vkQueueWaitIdle` (proste, wolniejsze) — do M5a.
- **Opóźnienie:** NVENC ULL + zerolatency + intra refresh ≈ **1–3 ms** na klatkę 1080p, plus konwersja BGRA→NV12 (~0,3 ms shader, ~2 ms CPU w M5a) i transfer host↔device (~1–2 ms w M5a). Realistycznie **M5a: 5–8 ms**, **docelowo zero-copy: 2–4 ms**.

**Werdykt (a):** **TAK, to jest wybór docelowy.** Dojrzałe, sterowane, niskie opóźnienie, HEVC na Ampere. Koszt: własne FFI + interop CUDA/dmabuf (trudne) albo kopia przez hosta (łatwe, na start).

### (b) Vulkan Video encode (`VK_KHR_video_encode_h265`)

**Czy działa na RTX 3090 Ti?** Vulkan Video encode H.264/H.265 jest wspierane przez sterownik NVIDIA od serii 5xx (Linux), ale **dojrzałość i wsparcie na Ampere bywa ograniczone** — encode H.265 przez Vulkan Video na Ampere jest w sterownikach NVIDIA obecne, ale wymaga sprawdzenia `vkGetPhysicalDeviceVideoCapabilitiesKHR` dla `VK_VIDEO_CODEC_OPERATION_ENCODE_H265_BIT_KHR` i profilu Main. **To trzeba zmierzyć, nie zakładać.** Rozszerzenia: `VK_KHR_video_queue`, `VK_KHR_video_encode_queue`, `VK_KHR_video_encode_h265`, plus `VK_KHR_video_maintenance1` (jeśli dostępne).

**Ile pracy:** dużo. Trzeba samemu generować:
- VPS/SPS/PPS (z `StdVideoH265VideoParameterSet`, `StdVideoH265SequenceParameterSet`, `StdVideoH265PictureParameterSet` — to struktury z `vulkan_video_codecs_common.h`, część Vulkan-Headers, licencja Apache-2.0 — **OK**),
- nagłówki slice (`StdVideoEncodeH265SliceHeader`), decyzje o typie slice, QP,
- rate control — **Vulkan Video nie ma rate control w sterowniku w prosty sposób**; `VK_KHR_video_encode_h265` ma `VkVideoEncodeH265RateControlInfoKHR` (rozszerzenie `VK_KHR_video_encode_h265` + `VK_EXT_video_encode_h264`/h265 rate control), ale konfiguracja jest żmudna i słabo udokumentowana. Realnie: **tygodnie pracy** i ryzyko, że na Ampere coś nie zadziała.

**Zalety:** zero kopii (renderujemy do `VkImage` z `VK_VIDEO_ENCODE_H265` usage, kodujemy bezpośrednio), wszystko w Vulkanie, który już mamy, brak zależności od CUDA i licencji SDK.

**Opóźnienie:** porównywalne z NVENC (ten sam sprzęt), ~2–4 ms.

**Werdykt (b):** **Odłożone.** Wysokie ryzyko, dużo pracy, niepewne wsparcie na Ampere. **Ale:** zrób 30-minutowy eksperyment — `vulkaninfo`/mały program sprawdzający `vkGetPhysicalDeviceVideoCapabilitiesKHR` dla encode H.265 na 3090 Ti. Jeśli działa i da się skonfigurować rate control — to jest lepsza droga długoterminowo (zero kopii, brak CUDA). Do M5 nie wchodzimy.

### (c) x265 (programowe)

**Zapas.** x265 jest GPL — **odpada jako zależność** (MIT/Apache/BSD only). Można by użyć `libx265` przez dlopen (nie linkujemy, nie vendorujemy), ale GPL i tak jest problematyczna przy dystrybucji. **Werdykt (c):** tylko jako **narzędzie pomiarowe** (referencja jakości/bitrate), nie w produkcie. Opóźnienie: 10–30 ms na 1080p (medium preset), nieakceptowalne dla gry.

**Rekomendacja:** **(a) NVENC, ścieżka przez hosta w M5a, zero-copy w M5b.** Równolegle eksperyment (b) jako opcja przyszłościowa.

---

## 2. Klient: dekodowanie HEVC na Intel HD 520

**VA-API (libva, dlopen, własne FFI) — wybór.** Skylake Gen9 ma sprzętowy dekoder HEVC 8-bit (Main). `vainfo` to potwierdzi; jeśli nie, fallback na software (libavcodec — ale to LGPL, dlopen, tylko jako fallback pomiarowy).

**Wywołania:**
```
vaGetDisplayDRM(fd) lub vaGetDisplayWayland(display)  // DRM prostsze: /dev/dri/renderD128
vaInitialize(dpy, &major, &minor)
vaQueryConfigProfiles → VAProfileHEVCMain
vaCreateConfig(dpy, VAProfileHEVCMain, VAEntrypointVLD, attrs, n, &config)
vaCreateSurfaces(dpy, VA_RT_FORMAT_YUV420, w, h, &surfaces, n, {VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2})
vaCreateContext(dpy, config, w, h, flags, surfaces, n, &ctx)
// per frame:
vaBeginPicture(dpy, ctx, surface)
vaRenderPicture(dpy, ctx, &{VAPictureParameterBufferHEVC, VASliceParameterBufferHEVC, VAIQMatrixBufferHEVC, slice data})
vaEndPicture(dpy, ctx)
vaSyncSurface(dpy, surface)
```

**Czy trzeba parsować nagłówki slice samemu?** **Tak, minimalnie.** VA-API nie parsuje bitstreamu — dostajesz surowe NAL-e i musisz wypełnić `VAPictureParameterBufferHEVC` (PPS/SPS-derived: `pic_width_in_luma_samples`, `bit_depth`, `chroma_format_idc`, listy referencyjne, `num_ref_idx`, `slice_parsing_fields` itd.), `VASliceParameterBufferHEVC` (offset/length slice data w buforze, `slice_type`, `slice_qp_delta`, `num_entry_point_offsets`…) oraz `VAIQMatrixBufferHEVC` (macierz skwantyzowana — można pominąć, jeśli `pic_scaling_list_enabled_flag=0`; wtedy `vaRenderPicture` z samym `VAPictureParameterBufferHEVC` + `VASliceParameterBufferHEVC` + danymi). **Minimalny parser:** NAL splitter (start code `00 00 01`), ekspander emulation prevention (`00 00 03` → `00 00`), bitreader, parser VPS/SPS/PPS/slice header **tylko pól wymaganych przez VA-API**. To jest **dużo pracy, ale dobrze zdefiniowane** — idealne dla małego modelu z testami (patrz §5).

**Alternatywa: Vulkan Video decode na Intelu.** Mesa `anv` ma `VK_KHR_video_decode_h265` dla Gen9? **Sprawdź `vulkaninfo` na laptopie** — dla Skylake wsparcie bywa niepełne (Gen9 ma HEVC decode, ale Vulkan Video decode w anv dojrzewało później; na Gen9 może brakować). Jeśli jest — to prostsze (mamy już Vulkan), ale ryzyko. **Rekomendacja: VA-API jako główna, Vulkan Video decode jako eksperyment.**

**Wyświetlanie NV12 → ekran:**
- **Docelowo (zero-copy):** `vaExportSurfaceHandle(dpy, surface, VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2, ...)` → dmabuf (Y + UV, z `VADRMPRIMESurfaceDescriptor`), import do `VkImage` przez `VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier` (format `VK_FORMAT_G8_B8R8_2PLANE_420_UNORM`), konwersja NV12→BGRA w shaderze (fragment shader z dwoma samplerami: Y i UV, `yuv_to_rgb` BT.709), `vkCmdCopyImage`/render do swapchaina Intela. **Opóźnienie: ~1–2 ms.**
- **Pośrednio (M5a, prostsze):** `vaGetImage` do CPU (NV12), konwersja NV12→BGRA na CPU (prosta pętla, ~2–3 ms dla 1080p), obecna ścieżka `vkCmdCopyBufferToImage`. **Opóźnienie: ~4–6 ms.** To jest ścieżka na pierwszy kamień milowy — **nie zmieniamy reszty protokołu**, tylko podmieniamy dekoder.

**Rekomendacja:** **M5a: VA-API decode + `vaGetImage` + CPU NV12→BGRA + obecna ścieżka.** M5b: dmabuf zero-copy.

---

## 3. Protokół

**Pakowanie w istniejące ramki.** Nagłówek 28 B już ma `codec`. Ustawiamy `codec=Hevc`. **Jedna jednostka dostępu (access unit) na ramkę** — NVENC z `enablePTD=1` i bez B-ramek daje dokładnie jeden AU na `nvEncEncodePicture`. Payload = surowe NAL-e (VPS/SPS/PPS + slice) **bez** start code'ów? **Decyzja: z start code'ami** (`00 00 01`) — prostsze, VA-API i tak je parsuje; narzut 3 B/NAL, pomijalny.

**Parametry VPS/SPS/PPS:** pierwsza ramka po inicjalizacji enkodera i **każda ramka IDR** zawiera VPS/SPS/PPS (NVENC robi to domyślnie przy IDR, jeśli `NV_ENC_CONFIG_HEVC.repeatSPSPPS=1`). Klient przy każdej ramce sprawdza, czy payload zaczyna się od VPS/SPS/PPS, i jeśli tak — rekonfiguruje VA-API (`vaDestroyContext`/`vaCreateContext` przy zmianie rozmiaru, inaczej tylko aktualizuje bufory). **Nie** wysyłamy parametrów osobno — trzymamy się „jedna ramka = jeden AU".

**Żądanie IDR:** nowa ramka kontrolna w protokole (poza nagłówkiem wideo) — `FrameControl { kind: RequestIdr, reason: Loss|Resize|Start }`. Klient wysyła po: (a) utracie ramki (luka w `seq`), (b) zmianie rozmiaru swapchaina, (c) starcie. Serwer przy następnym `nvEncEncodePicture` ustawia `NV_ENC_PIC_FLAG_FORCEIDR` i `repeatSPSPPS=1`. **Uwaga:** IDR = duża ramka (kluczowa), więc żądanie IDR ma priorytet, ale nie może zalać łącza — ogranicz do 1 na ~500 ms.

**Backpressure:** już jest (ramki >2 w kolejce). Dodajemy: przy zapełnionej kolejce serwer **nie koduje** nowej klatki (albo koduje z niższym bitrate — CBR sam się dostosuje), a klient **dropuje** najstarsze ramki P (nie IDR). Polityka: kolejka 2 ramek, drop najstarszej P, IDR zawsze dostarczany.

**Pomiar opóźnienia:** znaczniki czasu w nagłówku — rozszerzamy nagłówek o `capture_ts_ns` (monotoniczny zegar serwera przy `vkQueueSubmit`), `encode_done_ts_ns`, `recv_ts_ns` (klient), `present_ts_ns`. **Uwaga:** zegary różnych maszyn — potrzebna synchronizacja (NTP/PTP) albo mierzymy **szkło-do-szkła** przez zewnętrzną kamerę/`high-speed camera` na pierwszym etapie, a potem różnice po stronie klienta (`recv→present`) i serwera (`capture→encode_done`) osobno. **Rekomendacja:** najpierw mierzyć składniki lokalnie (serwer: capture→encode_done; klient: recv→present; sieć: RTT znane 0,2 ms), sumować; docelowo PTP.

---

## 4. Plan pomiaru

1. **Bitrate vs jakość.** Sceny: (a) `vkcube` (prosta, dużo płaskich obszarów — LZ4 wygrywa, HEVC też), (b) scena z teksturą (np. `vkcube` z teksturowaną kostką lub prosty render z teksturą 4K — **nie gra**, żeby pomiar był powtarzalny). Metryki: PSNR, SSIM (liczone na klatkach po dekodowaniu vs referencja z serwera przed kodowaniem), bitrate (Mbps), rozmiar klatki IDR vs P. **Cel:** 1080p60, ≤ 400 Mb/s **łącznie** (wideo + polecenia + różnice pamięci). Przy 60 FPS i 400 Mb/s → ~830 KB/klatkę → ~6,6 Mb/klatkę → HEVC 1080p przy ~6–8 Mbps na klatkę to **bardzo dobry budżet** (typowy streaming 1080p60 to 6–10 Mbps **na sekundę**, my mamy 6,6 Mb **na klatkę** — czyli ~400 Mbps to dużo). **Wniosek: 400 Mb/s jest osiągalne z zapasem; prawdziwym celem jest ≤ 100 Mb/s dla komfortu.** Zmierz i raportuj.
2. **Opóźnienie szkło-do-szkła.** Kamera 240 fps na ekranie + znaczniki; docelowo PTP. Składniki: capture→encode_done (serwer), sieć (RTT), recv→present (klient). Cel: < 20 ms end-to-end (producent referencyjny deklaruje płynność przy 1,5 ms RTT; my mamy 0,2 ms).
3. **CPU klienta.** `perf stat` na procesie ICD: % CPU na dekodowanie (VA-API sprzętowe powinno być < 5% na 1080p60), konwersja NV12→BGRA (CPU w M5a — zmierzyć, czy nie zjada rdzenia; jeśli tak, priorytetyzować dmabuf), present.
4. **Regresja:** ten sam test na Raw/LZ4/HEVC — tabela bitrate/opóźnienie/CPU.

---

## 5. Podział na moduły (kolejność, trudność, zlecalność)

**Zlecalne małemu modelowi (z gotowymi testami):**
1. **`gpu-hevc-nal`** — splitter NAL (start code, emulation prevention), bitreader, writer. Testy: round-trip na losowych danych, znane wektory. **Łatwe.**
2. **`gpu-hevc-params`** — parser VPS/SPS/PPS/slice header (tylko pola wymagane przez VA-API), mapowanie na `VAPictureParameterBufferHEVC`/`VASliceParameterBufferHEVC`. Testy: round-trip, porównanie z referencją (np. wygenerowaną przez `ffmpeg -bsf:v hevc_mp4toannexb` — tylko do testów). **Średnie, dobrze zdefiniowane.**
3. **`gpu-wire` rozszerzenia** — nowe pole `capture_ts_ns`, `FrameControl::RequestIdr`, polityka dropu. Testy: round-trip, symulacja kolejki. **Łatwe.**
4. **`gpu-hevc-testgen`** — generator scen testowych (vkcube, tekstura), skrypt pomiarowy PSNR/SSIM. **Łatwe.**

**Za trudne dla małego modelu (robimy sami, z testami integracyjnymi):**
5. **`gpu-nvenc`** — własne FFI (`#[repr(C)]` struktury, `dlopen`/`dlsym`), sesja, inicjalizacja, encode pętla, obsługa błędów wersji. **Trudne** (unsafe, wersjonowanie, brak nagłówków). Test: encode 1 klatki, dekodowanie przez `ffmpeg` (tylko weryfikacja), porównanie PSNR.
6. **`gpu-nvenc-interop`** — Vulkan→CUDA (dmabuf import, `cuImportExternalMemory`), konwersja BGRA→NV12 shaderem. **Bardzo trudne** (synchronizacja, dmabuf, CUDA). Najpierw kopia przez hosta (M5a), potem to.
7. **`gpu-vaapi`** — własne FFI VA-API (`dlopen`), sesja, konfiguracja, decode pętla, `vaExportSurfaceHandle`. **Trudne** (unsafe, dmabuf).
8. **`gpu-present-dmabuf`** — import dmabuf do `VkImage`, shader NV12→BGRA, present. **Trudne** (interop, format modifier).
9. **`gpu-latency`** — znaczniki czasu, PTP, pomiar. **Średnie.**

**Pierwszy najmniejszy kamień milowy (M5a) — HEVC end-to-end, bez zero-copy:**
- Serwer: `vkCmdCopyImageToBuffer` (już mamy) → CPU BGRA→NV12 → `cuMemcpyHtoD` → NVENC (własne FFI, `CUDEVICEPTR`) → NAL-e → istniejąca ramka z `codec=Hevc`.
- Klient: istniejący odbiór → VA-API decode (`vaGetImage` do CPU) → CPU NV12→BGRA → **obecna ścieżka** `vkCmdCopyBufferToImage` + present.
- **Kryterium:** `vkcube` wyświetla się z HEVC, bitrate ≤ 100 Mb/s, opóźnienie end-to-end < 30 ms, CPU klienta < 50% rdzenia. **Bez zmiany reszty protokołu** (tylko `codec` i nowe pole czasu).
- **Kolejność:** (1) `gpu-hevc-nal` + `gpu-hevc-params` (zlecalne) → (2) `gpu-nvenc` (my) → (3) `gpu-vaapi` (my) → (4) integracja + pomiar. **Szacunek: 2–3 tygodnie** (zależnie od doświadczenia z FFI).

**M5b (optymalizacja):** zero-copy po obu stronach (dmabuf + shader), IDR na żądanie, pomiar vs M5a. **M5c (opcjonalnie):** eksperyment Vulkan Video encode na 3090 Ti — jeśli działa, rozważ migrację.

**Ryzyka:** (1) VA-API na HD 520 — sprawdź `vainfo` **dziś**; jeśli brak HEVC Main, fallback na software (pomiarowy) i priorytet dmabuf. (2) NVENC wersja API vs sterownik — sprawdź `nvEncGetEncodeCaps` na starcie. (3) Licencja SDK — własne FFI z dokumentacji, komentarz o pochodzeniu, konsultacja przed publikacją. (4) Vulkan Video encode na Ampere — eksperyment 30 min, nie blokuje M5.
---

## Przegląd (Claude, 2026-11-02)

**Fakty zmierzone dziś, które zmieniają plan:**
- **Laptop (Intel HD 520, `i965`) NIE ma dekodowania HEVC.** `vainfo` listuje tylko H.264 (VLD i kodowanie), MPEG-2, VC-1, JPEG. Sterownik `iHD` (który na Skylake potrafi HEVC) nie ładuje się: `undefined symbol GmmGetAIL` (niezgodna wersja gmmlib w systemie, naprawa to zmiana pakietów systemowych, poza projektem). Odpowiedź DeepSeeka zakłada HEVC na Gen9 bez sprawdzenia; to był jego najpoważniejszy błąd.
- **Serwer .2:** `libnvidia-encode.so.1` i `libcuda.so.1` są (sterownik 580.178.04); brak `ffmpeg`.

**Decyzja: pierwszy kodek wideo to H.264 (AVC), HEVC później.** NVENC koduje H.264 dojrzale i z niskim opóźnieniem, a Intel dekoduje go sprzętowo przez VA-API (`VAProfileH264Main/High`, `VAEntrypointVLD`) już dziś. Koszt: H.264 potrzebuje ok. 30-40% większego bitrate niż HEVC przy tej samej jakości (przy 50-100 Mb/s to nadal bardzo dużo zapasu względem 1080p60). Pole `codec` dostaje wartość `H264 = 3`; `Hevc = 1` zostaje zarezerwowane, aż naprawimy iHD.

**Przyjęte z odpowiedzi:** NVENC przez `dlopen` z własnymi deklaracjami FFI (nagłówki SDK mają własną licencję, więc ich nie wklejamy; deklaracje piszemy z dokumentacji, komentarz o pochodzeniu); M5a przez kopię hosta (już mamy bufor odczytu); start code'y w strumieniu, jedna jednostka dostępu na ramkę, SPS/PPS powtarzane przy IDR; IDR na żądanie; x265 tylko jako narzędzie pomiarowe (GPL); VA-API do dekodowania z `vaGetImage` na start.

**Poprawki względem odpowiedzi:**
1. **NVENC przyjmuje wejście RGB (ARGB/ABGR) i sam robi konwersję kolorów.** Konwersja BGRA→NV12 na CPU (2 ms) w M5a jest zbędna: rejestrujemy bufor CUDA w formacie `NV_ENC_BUFFER_FORMAT_ARGB`/`ABGR` (sprawdzić, który odpowiada bajtom BGRA w pamięci) i wprost go kodujemy.
2. **Parser nagłówków można radykalnie uprościć, bo sami konfigurujemy koder.** Strumień ma stały profil: jeden slice na ramkę, bez B-ramek, 1 ramka odniesienia, bez macierzy skalujących, `frame_mbs_only`, stały SPS/PPS. Parser odrzuca wszystko inne zamiast obsługiwać cały standard. To zmniejsza ryzyko największego kawałka po stronie klienta (wypełnianie `VAPictureParameterBufferH264`/`VASliceParameterBufferH264`).
3. Rachunek bitrate w odpowiedzi jest pomylony (mieszanie Mb/s z Mb/klatkę). Poprawnie: 400 Mb/s przy 60 FPS to 6,7 Mb (830 KB) na klatkę, a typowy dobry strumień 1080p60 gry to 25-60 Mb/s. Prawdziwy cel to **<= 60-100 Mb/s łącznie**, a nie 400.
4. Wymagany jest kontekst CUDA (NVENC na Linuksie): FFI do `libcuda.so.1` (`cuInit`, `cuDeviceGet`, `cuCtxCreate_v2`, `cuMemAlloc_v2`, `cuMemcpyHtoD_v2`), też przez `dlopen`.
5. Vulkan Video encode: eksperyment 30 min jest sensowny, ale nie blokuje.

**Kolejność prac M5a (H.264):** (1) wektor testowy z libx264 + parser NAL/SPS/PPS/slice header (zlecany na .11 z czerwonymi testami); (2) `gpu-nvenc` (FFI, sam); (3) `gpu-vaapi` (FFI, sam); (4) integracja: kodek `H264` w `PresentFrame`/odbiorniku, żądanie IDR; (5) pomiar bitrate, opóźnienia, CPU. Dodatkowo (poza M5): poprawić lokalne wyświetlanie (20 ms/klatkę).
