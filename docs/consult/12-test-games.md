# Zestawienie gier i benchmarków do testów sterownika `gpu-icd`

## Kontekst sprzętowy i kryteria doboru

- **Klient:** Ryzen 5 4500U (iGPU Radeon), Ubuntu 26.04, ekran 1280×720, 6,6 GB dostępnego RAM (docelowo 16 GB), sieć 1 Gb/s (kabel).
- **Serwer:** RTX 3090 Ti (`gpu-renderd`).
- **Twarde wymagania ICD:** wyłącznie **64-bit (`x86_64`)** (brak 32-bitowego `libvulkan` ICD), brak jądrowego anty-cheatu, brak wymogu stałego połączenia z serwerami sieciowymi z agresywnym DRM, niskie zużycie RAM na kliencie (szczególnie przed rozbudową do 16 GB).
- **Już przetestowane / w toku:** *Cyberpunk 2077* (REDengine 4, DX12/vkd3d-proton), *American Truck Simulator* (Prism3D, natywny Vulkan), *Black Book* (Unity, DX11/DXVK), *Hamster Hunter* (Unity, Vulkan), *Dota 2* (Source 2, natywny Vulkan), *Rise of the Tomb Raider* (Foundation Engine, DX11/DX12/natywny Vulkan), *FurMark*, *GravityMark*, *vkmark*.

---

## Grupa A: Pobierz najpierw (maks. 8 pozycji — najlepszy stosunek pokrycia ścieżek sterownika do rozmiaru)

Te pozycje ważą łącznie **ok. 27,8 GB**, mieszczą się w 6,6 GB RAM na kliencie i pokrywają brakujące ścieżki w `gpu-icd`: ray tracing (`VK_KHR_acceleration_structure` / `VK_KHR_ray_tracing_pipeline`), Unreal Engine 4 (DX11 vs DX12 na tym samym tytule), Godot 4 (Forward+ Vulkan), `indirect draw` + `compute culling`, deterministyczne wbudowane benchmarki oraz mikro-testy pojedynczych rozszerzeń.

| # | Tytuł | Sklep / źródło | Cena | Rozmiar (GB) | Rozmiar łączny (GB) | Arch. | API gry -> ścieżka na Linuksie | Silnik | Co sprawdza w `gpu-icd` | Ryzyko / uwagi |
|---:|---|---|---|---:|---:|---|---|---|---|---|
| 1 | **Sascha Willems Vulkan Examples + Khronos Vulkan-Samples** | GitHub (darmowe) | free | ~0,8 | **0,8** | 64-bit | **Vulkan** -> natywnie (ELF x86_64) | Własny (C++) | Izolowane testy pojedynczych funkcji: `vkCmdDrawIndexedIndirect`, `push descriptors`, `descriptor buffer`, `timeline semaphores`, `dynamic rendering`, `compute` i `ray tracing` (każda próbka to osobny binarny test regresji). | Brak DRM/launchera; wymaga jednorazowego pobrania paczki zasobów (`assets`). |
| 2 | **Quake II RTX (Demo / Shareware)** | Steam (AppID 1089130) / GitHub | free | ~2,0 | **2,8** | 64-bit | **Vulkan** (`VK_KHR_ray_tracing_pipeline`, `VK_KHR_acceleration_structure`) -> natywnie (Linux lub Proton) | Q2VKPT (id Tech 2 + Vulkan Path Tracer) | Pełny potok **Ray Tracingu**: budowa i aktualizacja BLAS/TLAS co klatkę (`vkCmdBuildAccelerationStructuresKHR`), `vkGetBufferDeviceAddress`, `SBT` (Shader Binding Table), duże bufory `STORAGE_BUFFER` i odszumiacz w compute. | Darmowa wersja na Steamie zawiera 3 pierwsze poziomy bez DRM; działa płynnie w 1280×720. |
| 3 | **vkQuake** (+ dane Quake Shareware) | GitHub / pakiet systemowy | free | ~0,2 | **3,0** | 64-bit | **Vulkan** -> natywnie | id Tech / QuakeSpasm-Vulkan | Bardzo lekkie obciążenie CPU/RAM (<300 MB), tysiące drobnych wywołań `vkCmdBindPipeline`/`vkCmdPushConstants` na klatkę, dynamiczne bufory wierzchołków w pamięci `HOST_VISIBLE | HOST_COHERENT`, idealny do pomiaru czystego narzutu RTT i blokady `InstState`. | Zero DRM, startuje w <1 s, powtarzalny `timedemo demo1` do automatycznych pomiarów FPS. |
| 4 | **Road to Vostok (Public Demo)** | Steam (AppID 1963610) | free | ~3,5 | **6,5** | 64-bit | **Vulkan** (Forward+ Clustered) -> natywnie (Linux ELF) | **Godot 4.x** | Reprezentant silnika **Godot 4**: `VK_KHR_dynamic_rendering`, `compute` light clustering, SDFGI, intensywne użycie zestawów deskryptorów i barier pamięciowych (`vkCmdPipelineBarrier`). | Darmowe demo na Steamie, natywny 64-bitowy binariusz Linuksa, brak DRM. |
| 5 | **Unigine Superposition** | unigine.com (instalator `.run`) | free | ~1,8 (instalacja ~5,0) | **11,5** | 64-bit | **DX11** (Windows `.exe` -> Proton/DXVK -> Vulkan) lub **OpenGL 4.5** (Linux `.run` -> Zink -> Vulkan) | **Unigine 2** | Ciężkie cieniowanie SSRTGI, intensywne zapytania `VkQueryPool` (occlusion/timestamp queries), strumieniowanie tekstur, wbudowany deterministyczny przebieg benchmarku z wykresem czasów klatek (min/avg/max FPS). | Wersja darmowa (Basic) nie wymaga logowania; natywny binariusz Linux używa OpenGL (warto wymusić `MESA_LOADER_DRIVER_OVERRIDE=zink` albo uruchomić wersję Windows przez Proton+DXVK). |
| 6 | **The Talos Principle** | Steam / GOG | ~3–5 € (promocje) / ~28 € | ~6,0 | **17,5** | 64-bit | **Vulkan** -> natywnie (Linux 64-bit) lub Proton | **Serious Engine 4** | Wbudowany, w 100% powtarzalny **benchmark bez udziału gracza**; wielowątkowe nagrywanie buforów poleceń (`VkCommandBuffer` z kilku wątków roboczych), częste `vkWaitForFences` i zapytania o widoczność. | Natywna wersja Linux ma osobną binarkę 64-bit (`Talos_Unrestricted`); bardzo niskie zużycie RAM (~2 GB). |
| 7 | **Brotato** (lub **Cassette Beasts Demo**) | Steam / Epic / GOG | ~5 € (lub free z Epic) | ~0,8 | **18,3** | 64-bit | **Vulkan** -> natywnie (Linux) lub Proton | **Godot 3/4** (2D/3D batching) | Test skrajnie częstych małych zapisów do odwzorowanej pamięci co klatkę (tysiące sprite'ów/cząsteczek na ekranie), przebudowa pul deskryptorów, odtwarzanie dźwięku/UI bez wąskiego gardła GPU (pokazuje sufit FPS sterownika przy 1280×720). | Zużywa <600 MB RAM, brak launchera i DRM, natychmiastowy start. |
| 8 | **Ghostrunner** (lub **Ghostrunner Demo** na Steam) | Epic / GOG / Steam (często za darmo z rozdawnictwa Epic; Demo na Steam = free) | free (Demo / Epic) | ~9,5 (Demo) / ~22 (pełna) | **27,8** | 64-bit | **DX11** (DXVK) oraz **DX12 + Ray Tracing** (vkd3d-proton) — wybór przy starcie | **Unreal Engine 4** | Kluczowy test **Unreal Engine 4**: pozwala porównać tę samą scenę na **DX11 (DXVK)** i **DX12 (`VK_EXT_descriptor_buffer` w vkd3d-proton)** + opcjonalny Ray Tracing odbić/cieni, agresywne strumieniowanie tekstur UE4 i odtwarzanie wideo w menu (Media Foundation). | Brak DRM (na GOG/Epic działa offline przez Heroic); mieści się w 6,6 GB RAM przy 720p (w trybie DX11 zużywa ~3,8 GB RAM, w DX12 z RT ~5,2 GB RAM). |

---

## Grupa B: Pobierz później (gdy będzie 16 GB RAM lub po przejściu grupy A)

Pozycje bardzo wartościowe technicznie (kolejne silniki, duże strumieniowanie pamięci, async compute), ale cięższe na dysku (30–70 GB) lub wymagające >6,6 GB RAM na kliencie (szczególnie przez narzut pamięci cieniowej `Shadow`/`DoubleMap` w `gpu-icd`).

| # | Tytuł | Sklep / źródło | Cena | Rozmiar (GB) | Arch. | API gry -> ścieżka na Linuksie | Silnik | Co sprawdza w `gpu-icd` | Ryzyko / uwagi |
|---:|---|---|---|---:|---|---|---|---|---|
| 1 | **Shadow of the Tomb Raider (Free Trial / Pełna)** | Steam (darmowe Trial) / Epic / GOG | free (Trial) | ~38,0 | 64-bit | **Vulkan** (natywny port Feral) lub **DX12 + RT** (Proton/vkd3d) | Foundation Engine | Najlepszy wbudowany 3-scenowy benchmark (CPU vs GPU, wykresy czasów klatek), porównanie natywnego Vulkana Feral z DX12/vkd3d-proton oraz cieni Ray Traced. | Wersja Trial na Steamie jest darmowa i ma odblokowany benchmark; wymaga ok. 7–8 GB RAM przy dużych buforach. |
| 2 | **DOOM (2016) (Demo / Pełna)** | Steam | free (Demo) / ~4 € (promocja) | ~55,0 (pełna) | 64-bit | **Vulkan** -> Proton (64-bit) | **id Tech 6** | Wzorcowa implementacja Vulkana: **Async Compute** (osobne kolejki `VkQueue`), **MegaTexture** (ciągłe strumieniowanie małych stron tekstur do GPU), `vkCmdDrawIndexedIndirect`. | Dawne Demo na Steamie bywa ukryte w sklepie (`steam://install/479030` — wymaga weryfikacji dostępności na koncie); pełna gra tania na wyprzedażach, ma Denuvo (działa w Protonie). |
| 3 | **Control** | Epic / GOG / Steam | często free (Epic) / ~7 € | ~42,0 | 64-bit | **DX11** (DXVK) oraz **DX12 + pełny Ray Tracing** (vkd3d-proton) | **Northlight Engine** (Remedy) | Bardzo ciężki test `VK_EXT_descriptor_buffer` + 4 efektów Ray Tracingu naraz, masowe budowanie BLAS/TLAS dla fizyki zniszczeń, wiele wątków nagrywania. | Brak DRM na GOG/Epic; w trybie DX12 + RT potrafi przekroczyć 6,6 GB RAM na kliencie (lepie testować po rozszerzeniu do 16 GB). |
| 4 | **Metro Exodus Enhanced Edition** | Steam / GOG / Epic | ~6 € (promocja) | ~68,0 | 64-bit | **DX12 Ultimate** (wymaga sprzętu RT) -> vkd3d-proton | **4A Engine** | Silnik wymagający sprzętowego Ray Tracingu już na etapie inicjalizacji urządzenia (`VkPhysicalDeviceRayTracingPipelineFeaturesKHR`), osobny wbudowany konfigurator benchmarku. | Duży rozmiar; sprawdza kompletność zgłaszanych właściwości i rozszerzeń RT w `gpu-icd`. |
| 5 | **Strange Brigade** (lub **Zombie Army 4**) | Steam / GOG | ~3–5 € (promocja) | ~35,0 | 64-bit | **Vulkan** oraz **DX12** -> Proton | **Asura Engine** (Rebellion) | Natywny renderer Vulkan w silniku Asura, wbudowany deterministyczny benchmark, `async compute`, niskie wymagania względem RAM. | Czasem używa Denuvo (w zależności od wersji/sklepu). |
| 6 | **Counter-Strike 2** | Steam | free | ~40,0 | 64-bit | **Vulkan** -> natywnie (Linux 64-bit) | **Source 2** (nowa generacja) | Nowsza gałąź silnika Source 2 niż *Dota 2*: intensywne kompilowanie potoków (`VK_EXT_graphics_pipeline_library`), duże pule deskryptorów, wolumetryczny dym w `compute`. | VAC nie blokuje własnego ICD w grze z botami (`-insecure` w opcjach uruchamiania), ale do meczów online nie należy wchodzić z niestandardowym ICD. |
| 7 | **No Man's Sky** | Steam / GOG | ~20–24 € (promocja) | ~18,0 | 64-bit | **Vulkan** -> Proton | Własny silnik (Hello Games) | Gra czysto Vulkanowa: proceduralne generowanie geometrii i tekstur w `compute shaders`, tysiące buforów aktualizowanych w locie podczas przelotu nad planetą, długie sesje. | Brak DRM (na GOG), ale wymaga zakupu, jeśli nie ma jej w bibliotece. |

---

## Darmowe benchmarki i narzędzia testowe (podsumowanie dostępności na Linuksie)

> **Weryfikacja dostępności:**
> - **3DMark:** **nie ma wersji natywnej na Linuksa**. Działa jednak przez **Steam + Proton** (na Steamie dostępne jest darmowe **3DMark Demo**, AppID `231350`, rozmiar ~7–9 GB, 64-bit: zawiera *Time Spy* [DX12 -> vkd3d-proton], *Fire Strike* [DX11 -> DXVK] oraz *Steel Nomad Light* [DX12/Vulkan]).
> - **Basemark GPU:** oficjalne serwery pobierania (`gpuser分数` / PowerBoard) i repozytorium Flatpak dla darmowej wersji konsumenckiej zostały wygaszone przez Basemark (została tylko płatna wersja korporacyjna) — **trudno dostępny bez starego archiwum `.tar.gz` (~1,5 GB)**; funkcję tę lepiej pełni już przetestowany *GravityMark*.

| Benchmark | Licencja / źródło | Rozmiar (GB) | Arch. | API i ścieżka | Co testuje w `gpu-icd` |
|---|---|---:|---|---|---|
| **Sascha Willems Vulkan Examples** | Darmowy (MIT, GitHub) | ~0,5 | 64-bit | Natywny Vulkan | Ponad 100 pojedynczych binarek testujących po jednej funkcji API (compute, indirect, multithreading, ray tracing, descriptor indexing, dynamic rendering, subpasses). |
| **Khronos Vulkan-Samples** | Darmowy (Apache-2.0, GitHub) | ~0,5 | 64-bit | Natywny Vulkan | Oficjalne próbki wydajnościowe Khronosa (m.in. `descriptor_management`, `multithreaded_render_passes`, `hpp_texture_loading`, `ray_tracing`). |
| **Unigine Superposition** | Darmowy (Basic, unigine.com) | ~1,8 (pobr.) / ~5,0 (dysk) | 64-bit | DX11 (Proton/DXVK) lub OpenGL (Linux + Zink) | Ciężkie sceny oświetlenia dynamicznego, zapytania `VkQueryPool`, stabilność przy 100% obciążenia i ciągłym strumieniowaniu. |
| **Unigine Valley / Heaven** | Darmowy (unigine.com) | ~0,4–1,0 | 64-bit (Linux `.run`) / 32-bit (Windows `.exe`) | **Uwaga:** instalator Windows jest 32-bitowy (odpada pod DXVK!); wersja Linux ma binarkę 64-bitową OpenGL (działa z `gpu-icd` wyłącznie przez **Zink**: `MESA_LOADER_DRIVER_OVERRIDE=zink`). | Przydatny jako test warstwy **OpenGL -> Zink -> `gpu-icd`**. |
| **3DMark Demo (Steam)** | Darmowe Demo na Steamie | ~8,5 | 64-bit (subprocesy testów) | DX11 (DXVK) / DX12 (vkd3d-proton) / Vulkan | Branżowy standard porównawczy (*Time Spy*, *Fire Strike*, *Steel Nomad*); testuje tworzenie osobnego procesu renderującego z launchem z UI (wiele procesów/instancji Vulkan). |
| **glmark2** (przez Zink) | Darmowy (pakiet `glmark2-x11`) | <0,05 | 64-bit | OpenGL 2.1 -> **Zink** -> Vulkan | Szybki test kompatybilności naszego ICD z translatorem **Zink** (Mesa OpenGL-on-Vulkan). |
| **Blender Benchmark (CLI)** | Darmowy (`opendata.blender.org`) | ~0,8 | 64-bit | Vulkan (viewport backend w Blender 4.2+) | Test obciążeń nie-growych: kompilacja dużych shaderów, częste przełączanie potoków i wiele okien/swapchainów. |

---

## Grupa C: Odrzucone (nie pobierać — dlaczego odpadają)

| Tytuł / kategoria | Powód odrzucenia |
|---|---|
| **The Witcher 2 / klasyczny Skyrim (LE) / Fallout: New Vegas / GTA IV / Borderlands 2 (Windows) / Dishonored 1** | **Wyłącznie 32-bit (`i386`)**: nasz `gpu-icd` buduje się tylko jako 64-bitowy `cdylib` (`x86_64-linux-gnu`), więc proces 32-bitowy w Wine/Proton lub natywny 32-bit nie załaduje naszego sterownika. |
| **Unigine Heaven / Valley (wersje Windows `.exe`)** | Binarki Windows dla Heaven i Valley są **32-bitowe** (`x86`), więc uruchomione przez Proton/DXVK nie zobaczą 64-bitowego `gpu-icd` (używać wyłącznie 64-bitowej wersji Linux przez Zink albo *Unigine Superposition*, które jest 64-bitowe). |
| **Portal with RTX / Half-Life 2 RTX (RTX Remix)** | Architektura **RTX Remix Bridge** uruchamia oryginalną 32-bitową grę Source i mostkuje ją do 64-bitowego procesu renderera — wysokie ryzyko problemów z 32-bitowym loaderem oraz bardzo duże wymagania RAM (>12 GB) i dysku (25–40 GB). |
| **Fortnite / Valorant / Apex Legends / PUBG / Genshin Impact / EA FC / Battlefield V i 2042** | **Jądrowy lub inwazyjny anty-cheat** (Vanguard, Easy Anti-Cheat bez wsparcia Linux, BattleEye, EA Javelin) + wymóg stałego połączenia online; podmiana ICD przez `VK_ICD_FILENAMES` grozi blokadą uruchomienia lub flagowaniem konta. |
| **Red Dead Redemption 2 / GTA V / gry Ubisoft Connect i EA App** | Agresywne **zewnętrzne launchery** (Rockstar Games Launcher, Ubisoft Connect, EA App), które same tworzą dodatkowe procesy CEF/DirectX (często 32-bitowe procesy pomocnicze) oraz rozmiar >100 GB. |
| **Gry Unreal Engine 5 z dużym światem (np. *The Finals*, *Tekken 8*, *Black Myth: Wukong Benchmark*)** | Przy **6,6 GB RAM** na laptopie sam proces gry + kopie `Shadow`/`DoubleMap` dla odwzorowanych buforów w `gpu-icd` powodują natychmiastowy OOM (`Out of Memory`). Wrócić do *Black Myth: Wukong Benchmark Tool* (darmowy na Steamie, ~8 GB) dopiero po dołożeniu RAM do 16 GB. |

---

## Uwagi recenzenta (Claude, 2026-11-05): co w zestawieniu jest niepewne lub błędne

Zestawienie agy jest dobrym punktem wyjścia, ale jego dane NIE były weryfikowane z naszej strony; poprawki i zastrzeżenia:

- **Brotato i Cassette Beasts (poz. A7) nie nadają się**: oba używają Godota 3 (OpenGL ES 3), nie Vulkana. Godot 4 z Vulkanem ma np. Road to Vostok (A4); pozycję A7 skreślić.
- **Basemark GPU "wygaszony" jest nieprawdą**: wersję 1.2.3 pobraliśmy na Ryzena (~/Downloads, razem z benchmark-launcher); do przetestowania wymaga kliknięcia w launcherze.
- **Unigine Heaven/Valley "tylko 32-bit" jest wątpliwe**: paczki Windows zawierają binarki x64 (np. `Heaven_x64.exe`), więc Heaven przez Proton/DXVK nie jest z góry wykluczony (a Heaven przez Zink już mamy w planie). Sprawdzić na miejscu.
- **Quake II RTX, Control, Metro Exodus Enhanced**: wymagają `VK_KHR_ray_tracing_pipeline`/`acceleration_structure`; nasz ICD ogłasza rozszerzenia tylko wtedy, gdy wszystkie ich polecenia da się zakodować (`exts.rs`), więc najpierw trzeba sprawdzić, czy RT jest w ogóle ogłaszane. Bez tego gra odmówi startu albo wyłączy RT: to jest wartościowy wynik, ale nie "test wydajności".
- Rozmiary i rozmiar łączny (kolumna kumulatywna) są szacunkowe; AppID i dostępność dem (Ghostrunner, Road to Vostok, DOOM 2016) do potwierdzenia w sklepie przed pobraniem.
- Z tego, co już wiemy z testów: najwięcej błędów znajdujemy w grach, które tworzą kilka instancji/urządzeń naraz (DX11+DX12 w jednym procesie: Rise of the Tomb Raider), w pamięci bez cache (DXVK) i w wielowątkowym nagrywaniu (vkd3d). Kolejność pobierania warto układać pod te cechy.
