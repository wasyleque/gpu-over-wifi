<!-- i18n-switch -->
**English** · [Polski](TESTING-OTHER-MACHINE.md) · [Español](TESTING-OTHER-MACHINE.es.md)
<!-- /i18n-switch -->

# Test on another setup (Linux; Windows not yet)

Status as of 2026-11-02. **Linux as a client works** (measured on a laptop with Intel HD 520 + remote RTX 3090 Ti). **Windows is not yet supported**: it requires a separate ICD for Windows (stage M8: a different library name and a manifest in the registry, Win32 WSI, DXGI). The server (`gpu-renderd`) works on Linux with an NVIDIA, AMD, or Intel card with a Vulkan driver.

## 1. Server (machine with the card, Linux)
```
cargo build --release -p gpu-renderd          # albo cargo zigbuild --target x86_64-unknown-linux-gnu.2.39 dla starszego glibc
head -c 24 /dev/urandom | base64 > renderd-token && chmod 600 renderd-token
./target/release/gpu-renderd --token-file renderd-token --listen <ADRES-KABLA>:7979
```
By default it listens only on 127.0.0.1; you provide the network address explicitly (preferably only the Ethernet interface, not 0.0.0.0). The server user must belong to the `render` and `video` groups. In the daemon log, for each instance you can see which devices it detects (`widzi N urządzeń: [...]`); if only `llvmpipe` is visible, the GPU driver is not working for that process (restarting the daemon helps).

## 2. Client (machine on which the game is to run, Linux)
```
cargo build --release -p gpu-icd -p gpu-renderd
tools/icd-manifest.sh target/release/libgpu_icd.so ~/.local/share/gpu-hub/gpu_icd.json
scp serwer:renderd-token ~/.config/gpu-hub/renderd.token && chmod 600 ~/.config/gpu-hub/renderd.token
export GPU_ICD_ADDR=<ADRES-SERWERA>:7979 GPU_ICD_TOKEN_FILE=~/.config/gpu-hub/renderd.token
export VK_DRIVER_FILES=~/.local/share/gpu-hub/gpu_icd.json      # tylko nasz ICD jest widoczny dla aplikacji
```
The local card for DISPLAY is detected automatically (order: Intel, AMD, NVIDIA, other; lvp/virtio/gfxstream are skipped). Override: `GPU_ICD_LOCAL_ICD=/usr/share/vulkan/icd.d/<manifest>.json`.

## 3. Tests in order (from the simplest)
1. `target/release/gpu-probe <ADRES-SERWERA>:7979 --token-file <token>`: list of server cards and query time.
2. `tools/icd-smoke.sh <ADRES>:7979 <token> "<fragment nazwy karty>"`: `vulkaninfo --summary` sees the remote card.
3. `target/release/gpu-smoke 20`: buffer and image on the remote card, readback. `gpu-smoke surface` and `gpu-smoke swapchain`: surface and swapchain on a headless surface (without a window).
4. `GPU_ICD_STATS=1 vkcube --wsi wayland --c 600 --present_mode 0` (or `--wsi xcb`): cube on the remote card. First-frame capture: `GPU_ICD_DUMP_FRAME=/tmp/klatka.ppm`.

## 4. Options
- `GPU_ICD_CODEC=auto|raw|lz4` (currently raw and lz4 are available; `h264` and `hevc` are in the protocol and waiting for NVENC and a decoder; selecting an unavailable codec gives a warning and lz4).
- `GPU_ICD_SYNC_PRESENT=1`: synchronous mode (without a separate video connection), for diagnostics.
- `GPU_ICD_ALL_DEVICES=1`: also show the server's CPU devices (llvmpipe).
- `GPU_ICD_STATS=1`: statistics on stderr at shutdown; `GPU_ICD_DEBUG=1`, `GPU_ICD_DEBUG_LOCAL=1`: diagnostics.

## 5. What to send back after the test
- The `gpu-icd statystyki: ...` and `gpu-icd wideo: ...` lines (bytes per frame, stage times, network round trips).
- Local card model (`vulkaninfo --summary`), Mesa/driver version, compositor (Wayland/X11), link (Ethernet/Wi-Fi) and its RTT (`ping`).
- Result of `vainfo` (for selecting the H.264/HEVC codec on the client side).
- Whether the window shows the cube correctly; if not, the error message and `GPU_ICD_DEBUG=1`.

## 6. Known limitations
- The image comes back as H.264 on a separate connection (VA-API decoding on the client); the limiting factors are network round trips and client work, not the server (status and plan: `docs/consult/11-next-steps.md`).
- Games work (FurMark, ATS, Hamster Hunter, Cyberpunk 2077 via Proton; results in the README), but only on one client; tests on other machines: [`CALL-FOR-TESTERS.md`](CALL-FOR-TESTERS.md). Many Vulkan extensions are not advertised (rule: only what works).
- The daemon on one server was occasionally unstable when enumerating the NVIDIA card after a longer time (cause unexplained; see AGENTS.md).
- An authenticated client can send invalid Vulkan commands and hang the server driver: use a token and a trusted network.
