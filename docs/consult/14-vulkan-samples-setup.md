# Konfiguracja Khronos Vulkan-Samples na laptopie Ryzen (`192.168.1.211`)

## 1. Środowisko i rozmiary na `wasyl@192.168.1.211` (Ubuntu 26.04)

- **Dysk (`/`):** 190 GB wolnego z 468 GB (`/dev/nvme0n1p2`)
- **Pamięć RAM:** 6,6 GiB łącznie + 4,0 GiB Swap
- **Narzędzia:** `cmake 4.2.3`, `g++ 15.2.0`, `ninja 1.12.1`, `make 4.4.1`, `pkg-config 1.8.1`, `libvulkan-dev 1.4.341.0-1`, `glslangValidator`, nagłówki X11/XCB/Wayland — wszystkie zainstalowane.
- **Ścieżka repozytorium:** `/home/wasyl/VulkanSamples` (obok nienaruszonego `/home/wasyl/SaschaWillems`).
- **Ścieżka binarki Release:** `/home/wasyl/VulkanSamples/build/app/bin/Release/x86_64/vulkan_samples` (`21 MB`).
- **Rozmiary na dysku:**
  - Samo repozytorium po `git clone --recurse-submodules`: **ok. 3,0 GB** (głównie submoduł `assets` i `third_party`)
  - Katalog `~/VulkanSamples/build` (Release): **`509 MB`**
  - Łącznie `~/VulkanSamples` po zbudowaniu: **`3,5 GB`**

---

## 2. Wykonane polecenia instalacji i budowania

```bash
# 1. Klonowanie z submodułami
git clone --recurse-submodules https://github.com/KhronosGroup/Vulkan-Samples.git ~/VulkanSamples

# 2. Konfiguracja CMake (Release)
cd ~/VulkanSamples
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release

# 3. Kompilacja z limitem pamięci cgroup (ochrona 6,6 GB RAM przed swap thrashingiem na jednostkach vulkan.hpp)
systemd-run --user --scope -p MemoryHigh=4200M -p MemoryMax=4800M \
  nice -n 19 ionice -c 3 cmake --build build --config Release -- -j 2
```

### Uwagi eksploatacyjne
1. **Katalog roboczy (`cwd`):** `vulkan_samples` należy zawsze uruchamiać z katalogu głównego `/home/wasyl/VulkanSamples` (lub przekazać `--data-path`), aby względne ścieżki do `assets/` i `shaders/` działały poprawnie.
2. **Pamięć podczas przebudowy:** jednostki C++20 oparte na `vulkan.hpp` (`hpp_*.cpp`) zużywają w `g++ 15.2.0` do **1,35 GB RAM na jeden proces `cc1plus`**. Na laptopie z 6,6 GB RAM nie należy przekraczać `-j 2` (a przy działających w tle ciężkich aplikacjach jak Heroic/przeglądarka używać `-j 1`).
3. **Przydatne flagi CLI z `--help` do automatycznych testów `gpu-icd`:**
   - `./build/app/bin/Release/x86_64/vulkan_samples samples-oneline` — wypisuje wszystkie 114 wbudowanych próbek.
   - `--stop-after-frame <N>` — zamyka próbkę automatycznie po wyrenderowaniu `N` klatek.
   - `--benchmark` / `--log-fps` — loguje średni czas klatki / FPS.
   - `--headless-surface` — tworzy powierzchnię i swapchain przez `VK_EXT_headless_surface` bez otwierania okna X11/Wayland.
   - `--screenshot <frame> --screenshot-output <plik>` — zrzuca klatkę do pliku do weryfikacji poprawności renderowania.

---

## 3. Lista 10 próbek do testów `gpu-icd` (`vulkan_samples sample <nazwa>`)

Wszystkie poniższe identyfikatory zostały zweryfikowane wywołaniem `./build/app/bin/Release/x86_64/vulkan_samples samples-oneline`:

| # | Dokładne polecenie (`cd ~/VulkanSamples`) | Kategoria | Co weryfikuje w `gpu-icd` / `gpu-renderd` |
|---:|---|---|---|
| 1 | `./build/app/bin/Release/x86_64/vulkan_samples sample compute_nbody` | `api` | Potok `VkComputePipeline`, bufory `STORAGE_BUFFER`, synchronizacja i bariery między obliczeniami compute a grafiką. |
| 2 | `./build/app/bin/Release/x86_64/vulkan_samples sample multi_draw_indirect` | `performance` | `vkCmdDrawIndexedIndirect` / `vkCmdDrawIndexedIndirectCount`, bufory komend pośrednich czytane przez GPU. |
| 3 | `./build/app/bin/Release/x86_64/vulkan_samples sample descriptor_indexing` | `extensions` | `VK_EXT_descriptor_indexing` (bindless): tablice tekstur bez granic, `UPDATE_AFTER_BIND`, `PARTIALLY_BOUND`. |
| 4 | `./build/app/bin/Release/x86_64/vulkan_samples sample dynamic_rendering` | `extensions` | `VK_KHR_dynamic_rendering` (`vkCmdBeginRendering` / `vkCmdEndRendering` — podstawowa ścieżka `vkd3d-proton` i nowoczesnego DXVK). |
| 5 | `./build/app/bin/Release/x86_64/vulkan_samples sample timeline_semaphore` | `extensions` | `VK_KHR_timeline_semaphore`: `vkWaitSemaphores`, `vkSignalSemaphore`, `vkGetSemaphoreCounterValue` na CPU i GPU. |
| 6 | `./build/app/bin/Release/x86_64/vulkan_samples sample buffer_device_address` | `extensions` | `VK_KHR_buffer_device_address`: `vkGetBufferDeviceAddressKHR` i przekazywanie wskaźników GPU w buforach. |
| 7 | `./build/app/bin/Release/x86_64/vulkan_samples sample descriptor_buffer_basic` | `extensions` | `VK_EXT_descriptor_buffer`: `vkGetDescriptorEXT`, `vkCmdBindDescriptorBuffersEXT`, `vkCmdSetDescriptorBufferOffsetsEXT` (ścieżka `descsynth`). |
| 8 | `./build/app/bin/Release/x86_64/vulkan_samples sample ray_tracing_basic` | `extensions` | `VK_KHR_acceleration_structure` + `VK_KHR_ray_tracing_pipeline`: budowa BLAS/TLAS, tablica SBT, `vkCmdTraceRaysKHR`. |
| 9 | `./build/app/bin/Release/x86_64/vulkan_samples sample ray_queries` | `extensions` | `VK_KHR_ray_query`: śledzenie promieni inline bezpośrednio z shaderów. |
| 10 | `./build/app/bin/Release/x86_64/vulkan_samples sample multithreading_render_passes` | `performance` | Wielowątkowe nagrywanie wtórnych buforów poleceń (`VkCommandBuffer`) i `vkCmdExecuteCommands` (test kolejności `DispObj::pending`). |
