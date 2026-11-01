# gpu-over-wifi

Share one powerful home GPU between your devices: run big AI models, play games on a weak laptop as if it had the strong graphics card, and let the server decide who gets the GPU.

> **Status: early research project.** Things work in the lab, not yet in everyday use. Linux only for now (Windows client is planned).

## What is in here

- **GPU hub** (`hub-server`, `hub-client`, `hub-proto`) – a small server that watches the GPU, proxies AI requests (OpenAI-compatible API in front of Ollama) and decides who gets the GPU and when, so a big model and a game do not fight over VRAM. It can also run large models such as DeepSeek through [InferSSD](https://github.com/wici-ai/inferssd) (Apache-2.0) on a single consumer card by streaming weights from an NVMe drive.
- **Remote Vulkan** (`gpu-icd`, `gpu-renderd`, `gpu-vkgen`, `gpu-vkcodec`, `gpu-wire`, `gpu-proto`) – a from-scratch Vulkan driver for the *client* that forwards graphics commands over the network to a daemon on the *server*, which runs them on the real GPU and sends the picture back. To the application, the server's card looks like a locally installed GPU.
- **Video** (`gpu-nvenc`, `gpu-vaapi`, `gpu-h264`) – frames are returned either losslessly (LZ4) or as H.264 (hardware-encoded on NVIDIA, hardware-decoded through VA-API on the laptop).

## Where it stands

| Works | Not yet |
|---|---|
| Remote RTX appears in `vulkaninfo`; `vkcube` renders on it and shows locally | Real games (Proton/DXVK), page-diff memory sync, frame pacing |
| LZ4 and H.264 frame transport, selectable (`GPU_ICD_CODEC`) | Windows client driver, HEVC |
| AI proxy with VRAM-aware scheduling, DeepSeek via InferSSD | Game-session reservations in the scheduler |

## Try it

See [`docs/TESTING-OTHER-MACHINE.md`](docs/TESTING-OTHER-MACHINE.md) for building the server daemon and the client driver and running `vkcube` through them. Design notes: [`docs/DESIGN-remote-gpu.md`](docs/DESIGN-remote-gpu.md), [`docs/DESIGN-game-streaming.md`](docs/DESIGN-game-streaming.md). The working status log is [`AGENTS.md`](AGENTS.md).

## Security

The remote GPU daemon is **not hardened for untrusted networks**. A client with the token can run arbitrary Vulkan commands on the server's GPU, and a malformed command stream can crash the vendor driver. Use it only on a trusted link (e.g. a direct cable or a VPN), bind it to that interface only, and keep the token file private (`chmod 600`).

## Third-party material

`third_party/vk.xml` and the Vulkan headers come from Khronos (Apache-2.0); `third_party/nvenc/nvEncodeAPI.h` comes from `nv-codec-headers` (MIT-style notice kept in the file, see its README). VA-API types are generated from the system `libva` headers (MIT).

## License

Apache-2.0, see [`LICENSE`](LICENSE).
