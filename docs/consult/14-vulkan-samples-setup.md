# Konfiguracja Khronos Vulkan-Samples na laptopie Ryzen (`192.168.1.211`)

## 1. Wynik weryfikacji środowiska na `wasyl@192.168.1.211` (Ubuntu 26.04)

Zasoby dyskowe i pamięć przed rozpoczęciem:
- **Dysk (`/`):** 190 GB wolnego z 468 GB (`/dev/nvme0n1p2`)
- **Pamięć RAM:** 6,6 GiB łącznie (~3,0 GiB dostępne) + 4,0 GiB Swap

### Stan narzędzi i pakietów deweloperskich

| Pakiet / narzędzie | Status na Ryzenie | Ścieżka / wersja |
|---|---|---|
| `git` | **Zainstalowany** | `/usr/bin/git` |
| `cmake` | **BRAK (`MISSING`)** | — *(blokuje konfigurację i budowanie)* |
| `ninja-build` | **Zainstalowany** | `/usr/bin/ninja` |
| `make` | **Zainstalowany** | `/usr/bin/make` |
| `g++` | **Zainstalowany** | `/usr/bin/g++` |
| `pkg-config` | **Zainstalowany** | `/usr/bin/pkg-config` |
| `libvulkan-dev` | **Zainstalowany** | `1.4.341.0-1` (`amd64`) |
| `glslang-tools` | **Zainstalowany** | `/usr/bin/glslangValidator` |
| `libx11-dev`, `libx11-xcb-dev`, `libxcb1-dev` | **Zainstalowane** | `2:1.8.13-1ubuntu0.1`, `1.17.0-2ubuntu1` |
| `libxrandr-dev`, `libxinerama-dev`, `libxcursor-dev`, `libxi-dev` | **Zainstalowane** | nagłówki X11 wymagane przez GLFW obecne |
| `libwayland-dev`, `libxkbcommon-dev` | **Zainstalowane** | `1.24.0-2`, `1.13.1-1` |
| `wayland-protocols` | **BRAK (`MISSING`)** | — *(wymagany, gdy CMake/GLFW generuje nagłówki Wayland)* |

> **Uwaga:** Zgodnie z instrukcją zatrzymano działanie na kroku (1) bez uruchamiania `sudo` i bez klonowania/budowania. Również katalog `~/SaschaWillems` nie jest jeszcze obecny w `~` na Ryzenie (w `~` znajdują się obecnie `~/vkquake` oraz `~/gpu-hub-client`).

---

## 2. Polecenie instalacji brakujących pakietów (dla użytkownika na Ryzenie)

Uruchom na laptopie (`wasyl@192.168.1.211`):

```bash
sudo apt update && sudo apt install -y cmake wayland-protocols
```

---

## 3. Polecenia do wykonania po doinstalowaniu `cmake` (kroki 2 i 3)

Po zainstalowaniu pakietów powyższym poleceniem będzie można wykonać klonowanie i kompilację:

```bash
# Krok 2a: Klonowanie z submodułami (docelowy rozmiar repozytorium + assets + third_party to ok. 2,2–2,5 GB)
git clone --recurse-submodules https://github.com/KhronosGroup/Vulkan-Samples.git ~/VulkanSamples
du -sh ~/VulkanSamples

# Krok 2b: Konfiguracja i budowanie Release (Ninja, -j4 lub -j2 przy presji na 6,6 GB RAM)
cmake -S ~/VulkanSamples -B ~/VulkanSamples/build -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build ~/VulkanSamples/build --config Release -j 4

# Krok 3: Sprawdzenie listy próbek
~/VulkanSamples/build/app/bin/Release/x86_64/vulkan_samples --help
```

### 10 próbek `Vulkan-Samples` najbardziej przydatnych do testów `gpu-icd`

*(Nazwy identyfikatorów przekazywane do `./build/app/bin/Release/x86_64/vulkan_samples sample <nazwa>`):*

| # | Identyfikator próbki (`sample <nazwa>`) | Kategoria | Co weryfikuje w `gpu-icd` i `gpu-renderd` |
|---:|---|---|---|
| 1 | `compute_nbody` | `api` | Potok `VkComputePipeline`, pamięć `STORAGE_BUFFER`, bariery między kolejkami/etapami compute i grafiką. |
| 2 | `instancing` *(oraz `indirect_drawing` w SaschaWillems)* | `api` | Wielokrotne rysowanie instancjonowane, bufory wierzchołków i indeksów w pamięci urządzenia. |
| 3 | `descriptor_indexing` | `extensions` | `VK_EXT_descriptor_indexing`: tablice bez granic (bindless), `PARTIALLY_BOUND_BIT`, `UPDATE_AFTER_BIND_BIT`. |
| 4 | `dynamic_rendering` | `extensions` | `VK_KHR_dynamic_rendering` (`vkCmdBeginRendering` / `vkCmdEndRendering` używane intensywnie przez `vkd3d-proton`). |
| 5 | `timeline_semaphore` | `extensions` | `VK_KHR_timeline_semaphore`: `vkWaitSemaphores`, `vkSignalSemaphore`, `vkGetSemaphoreCounterValue` pomiędzy kolejkami. |
| 6 | `buffer_device_address` | `extensions` | `VK_KHR_buffer_device_address`: `vkGetBufferDeviceAddressKHR` i wskaźniki GPU przekazywane w buforach/push constants. |
| 7 | `push_descriptors` | `extensions` | `VK_KHR_push_descriptor` (`vkCmdPushDescriptorSetKHR` — test wykluczenia z bezblokadowej ścieżki `fast_table` ze względu na uchwyty w `VkWriteDescriptorSet`). |
| 8 | `ray_tracing_basic` | `extensions` | `VK_KHR_acceleration_structure` + `VK_KHR_ray_tracing_pipeline`: budowa BLAS/TLAS, Shader Binding Table, `vkCmdTraceRaysKHR`. |
| 9 | `ray_queries` | `extensions` | `VK_KHR_ray_query`: śledzenie promieni bezpośrednio z poziomu shaderów fragmentów/compute. |
| 10 | `multithreaded_render_passes` | `performance` | Równoległe nagrywanie `VkCommandBuffer` (w tym wtórnych CB i `vkCmdExecuteCommands`) na wielu wątkach roboczych. |
