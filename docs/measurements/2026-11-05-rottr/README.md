# Rise of the Tomb Raider (Epic, Proton, DX11 via DXVK) on the remote RTX 3090 Ti, 2026-11-05/06

Ryzen 5 4500U laptop, 1 Gb/s cable, H.264, built-in benchmark of the game, everything on high, DX12 off.
Screenshots `tr1.png`-`tr3.png` were taken around the end of the benchmark runs (23:53-23:58) and are not attributed to a particular run; game FPS counter top-left: 18.6 and 8.5 in two scenes. Their size is the size of the desktop, not the render resolution.

| Run | Build | Frames | Server wait | FPS (5 s samples) |
|---|---|---|---|---|
| 720p, first working run | before GPU-copy of whole memories | 4 139 | 798 s (193 ms/frame) | steady 7-8 |
| Full HD, high | + GPU copy for whole memories, multi-instance fix | 3 429 | 111 s (32 ms/frame) | 25-66 early, 6-13 in the heavy scenes, mean ~20 |
| 720p, same settings | same | ~2 400 | 83 s | 27-44 early, 6-14 later, mean ~20 |

Full HD at high settings gave **1-2 FPS** in the very first attempt (before this day's fixes).

## What was fixed on the way

1. The game creates several Vulkan instances in one process (DXVK probes, vkd3d-proton, the real D3D11 device). The driver kept
   one global "current" instance, so after one was destroyed the next `vkCreateDevice` returned `-3` without ever reaching the server
   (no message anywhere). Every call is now routed to the instance that owns its handle.
2. `MemReadDelta` (server, 314 s of 4 139 frames) read 16 MB device-local memories whole through the slow BAR window; they are now
   copied by the GPU into host-cached RAM first (also for whole-memory refreshes, not only ranges).
3. Pure capability queries (`vkGetPhysicalDeviceImageFormatProperties2`: 15 297 calls x 13 ms in the first run) are cached on the
   client. No cache-hit line appeared in this run's log, so the effect of this one is not confirmed.

## What the two resolutions say

The mean frame rate is the same at 1280x720 and 1920x1080 (about 20 FPS): the limit is not rendering or the link but synchronisation.
Remaining costs (Full HD run): `vkWaitSemaphores` 16 901 x 5 ms, queueing for the instance lock 90 s, `vkGetDescriptorEXT` 5 478 misses x 3.3 ms.
The GPU stays below 5 % busy.

## Run 3: compression moved off the instance lock (same Full HD high settings, shorter run, 1 854 frames)

| | before | after |
|---|---|---|
| instance lock held | 57.0 s / 3 429 frames = 16.6 ms per frame | 19.4 s / 1 854 frames = **10.4 ms per frame** |
| waiting for the instance lock | 89.6 s = 26.1 ms per frame | 29.2 s = **15.7 ms per frame** |
| mean FPS | 20.8 | 20.6 |

The lock is clearly shorter, but the mean frame rate did not move: the remaining wait (about 45 ms per frame in this run, mostly
`vkWaitSemaphores` 11 744 x 5.0 ms) comes from queueing behind memory reads and command batches on the single-threaded server connection.
