# Help us test: remote GPU for games on weak machines

We are looking for people with **different laptops, desktops and networks** who can run a game through the remote Vulkan driver and send back
numbers. Today everything was measured on one client (a Ryzen 5 4500U laptop) and one server (RTX 3090 Ti), so we do not yet know how the stack behaves
on other CPUs, GPUs, link speeds, window systems or games. Every report, including "it crashed at once", is useful.

## What works today

* Native Linux Vulkan games and Proton games (DirectX 12 through vkd3d-proton, DirectX 11 through DXVK) on a **Linux client**, rendered on an NVIDIA server GPU,
  picture returned as H.264 (hardware decode through VA-API on the client) or lossless LZ4.
* Measured so far: FurMark 93 FPS, American Truck Simulator 70 FPS (light scenes, 6 FPS in a busy cockpit scene), Hamster Hunter 21-34 FPS,
  Cyberpunk 2077 about 8 FPS (playable, 38 FPS in quiet scenes). Details: [README](../README.md#real-applications-ryzen-5-4500u-laptop---server-a-1-gbs-cable-h264-lab-runs).
* Not working yet: Windows clients, non-NVIDIA servers, OpenGL-only games (Zink crashes), multiple clients on one GPU at once (one session at a time is safe).

## What you need

| | Server (has the strong GPU) | Client (the weak machine) |
|---|---|---|
| OS | Linux | Linux (Ubuntu 26.04 and Arch/Omarchy tested), Wayland or X11 |
| GPU | NVIDIA with Vulkan (driver 580 tested), user in groups `render` and `video`; NVENC for H.264 | any GPU with VA-API H.264 decode (Intel, AMD) or use the LZ4 codec |
| Network | a **direct 1 Gb/s cable** (or better) between the two machines; Wi-Fi is not recommended (every frame waits for ~50 round trips) | same |
| RAM | 8 GB+ | 8 GB minimum, **16 GB recommended** (the driver keeps copies of the game's mapped memory) |
| Build | recent stable Rust (we build with 1.99) | recent stable Rust (we build with 1.99) |

Round-trip time matters more than bandwidth: a 0.15 ms cable gave 3x the frame rate of a 0.4 ms USB adapter in our tests.

## How to run a test (30 minutes)

1. Follow [`TESTING-OTHER-MACHINE.md`](TESTING-OTHER-MACHINE.md) to build and start the server daemon and the client driver and to run `vkcube`.
2. For a game, set the Steam launch options (or environment) like this on the client. `VK_LOADER_DRIVERS_SELECT` matters: a value inherited from Steam can hide the driver.
   ```
   VK_DRIVER_FILES=$HOME/.local/share/gpu-hub/gpu_icd.json VK_LOADER_DRIVERS_SELECT=*gpu_icd* \
   GPU_ICD_ADDR=<server>:7979 GPU_ICD_TOKEN_FILE=$HOME/.config/gpu-hub/renderd.token \
   GPU_ICD_CODEC=h264 GPU_ICD_STATS=periodic GPU_ICD_LOG=$HOME/gpu-hub-icd.log %command%
   ```
   For Proton games that start a launcher add the game's skip-launcher argument (Cyberpunk: `--launcher-skip -skipStartScreen`).
3. Confirm the game really uses the server GPU: the game's own log or `vulkaninfo --summary` must list the server GPU, and `nvidia-smi` on the server must show load.
4. Play 5-10 minutes in a repeatable place (same scene each time), then close the game normally.
5. Run `tools/collect-report.sh <server-address> [path-to-icd-log]` and attach the output to a new issue using the **Test report** template.

## What the report contains (nothing secret)

Client and server hardware and OS, the link's round-trip time and speed, the driver version, and the driver's own statistics: FPS per 5 s window, round
trips per command, lock contention, memory upload volume, and the watchdog report if a thread got stuck. No tokens, no file paths beyond the log name, no screenshots.
Please also tell us the game, its graphics settings and resolution, and how it felt (the number is not everything).

## If something goes wrong

* **Black window or "not responding" for a minute at start:** wait, loading is slow at first (shaders). Do not kill the game before 2 minutes.
* **The game exits with a watchdog or crash message after loading a save:** attach the game's own crash report and the driver log (the `WATCHDOG:` lines say which thread waited for what).
* **The server GPU memory stays used after a crash:** restart the daemon (`pkill gpu-renderd`, start it again).
* **Nothing connects:** check the token file permissions and that the server listens on the cable's address, not on 0.0.0.0 (see the [security note](../README.md#security)).

## Licence and safety

The code is source-available under FSL-1.1-ALv2 (free for any non-competing use, Apache-2.0 after two years), see [`LICENSE`](../LICENSE). The daemon is
**not hardened for untrusted networks**: use a direct cable or a VPN only, and keep the token private.
