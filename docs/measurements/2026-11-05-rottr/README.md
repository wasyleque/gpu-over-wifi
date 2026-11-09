# Rise of the Tomb Raider (Epic, Proton, DX11 via DXVK) on the remote RTX 3090 Ti, 2026-11-05/06

> **Correction (tester, 2026-11-06):** the 25-66 FPS samples in the tables below are the **start screen / menu**, not gameplay. Benchmark
> scene 1 runs at **10-13 FPS** and scene 2 at **6-8 FPS** (the tester closes the game there). The "mean ~20 FPS" figures therefore mix the menu
> with the benchmark and overstate the game; compare runs by scene, not by mean.

Ryzen 5 4500U laptop, 1 Gb/s cable, H.264, built-in benchmark of the game, everything on high, DX12 off.
Screenshots `tr1.png`-`tr3.png` were taken around the end of the benchmark runs (23:53-23:58) and are not attributed to a particular run; game FPS counter top-left: 18.6 and 8.5 in two scenes. Their size is the size of the desktop, not the render resolution.

| Run | Build | Frames | Server wait | FPS (5 s samples) |
|---|---|---|---|---|
| 720p, first working run | before GPU-copy of whole memories | 4 139 | 798 s (193 ms/frame) | steady 7-8 |
| Full HD, high | + GPU copy for whole memories, multi-instance fix | 3 429 | 111 s (32 ms/frame) | 25-66 early, 6-13 in the heavy scenes, mean ~20 |
| 720p, same settings | same | ~2 400 | 83 s | 27-44 early, 6-14 later, mean ~20 |

Full HD at high settings gave **1-2 FPS** in the very first attempt (before this day's fixes).

## Full built-in benchmark result (tester, 2026-11-06, build 0.3.7)

`tr_bench_results_1.png` (game's own result screen):

| Scene | FPS | min | max |
|---|---|---|---|
| Mountain Peak | **14.18** | 3.11 | 35.86 |
| Syria | **8.32** | 2.20 | 34.15 |
| Geothermal Valley | **7.86** | 1.35 | 49.52 |
| **Overall** | **9.59** | | |

(The settings of this run are the tester's usual Full HD / high, DX11; the very first attempt on the first build gave 1-2 FPS.)
The minima are the start of each scene (asset streaming); the maxima are the cut-scene-like intro seconds.

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

## Runs 4 and 5: dedup measurement, and zstd for memory writes (2026-11-09, same Full HD high settings)

Game result screens, same scenes (`tr_bench_results_*.png`; run 3 used the unsafe diagnostic `GPU_ICD_NO_REFRESH_UNCACHED=1` and is not comparable):

| run | what was different | Mountain Peak | Syria | Geothermal | Overall |
|---|---|---|---|---|---|
| 1 | baseline (LZ4) | 14.18 | 8.32 | 7.86 | 9.59 |
| 2 | LZ4 | 14.17 | 8.47 | 8.63 | 10.02 |
| 4 | LZ4, with `GPU_ICD_DIFFSTAT` and sample dump (measurement overhead) | 14.35 | 8.36 | 8.95 | 10.17 |
| 5 | **zstd level 1 for memory writes** (`MemWriteZstd`) | 14.07 | 8.84 | **9.88** | **10.67** |

Run-to-run spread between runs 1 and 2 (same build) is about 4 %, so the improvement of run 5 (+5 % overall, +10 % in Geothermal Valley,
the scene whose link was saturated) is consistent with the byte reduction but is a single run, not a proof.

**What was measured about the traffic (client log, whole process):**

* Content-hash cache potential (4 KiB windows already sent *anywhere* earlier): only 9-11 % of 21 GB, and 8-10 % of that is content that also
  differs from the previous content of the same place; zero pages are < 1 %. Unlike model weights in an inference workload, game uploads are
  mostly new data every frame, so a persistent content cache is not the big win here. Not implemented.
* 62 % of the bytes sent are identical to what was last sent to the same place (only 38 % of bytes, 52 % of 64 B lines changed). Sending only
  the changed lines (`MemWriteRuns`) is sound only if the GPU cannot write that memory: the server memory then differs from the client's
  belief. The usage flags vkd3d-proton gives its big buffers (`0x219ff`) include storage/transfer-destination bits, and the biggest memories
  are images or whole-memory writable, so the static "GPU cannot write" test qualified none of them; runs stay an opt-in, unused feature.
* Codec comparison on 1.16 GB of real sent chunks (independent 64 KiB blocks): LZ4 456 MB (2.54x), zstd-1 379 MB (3.05x, -17 %), zstd-3 373 MB.
  Per memory: 0x100046 4.3x -> 6.6x, 0x10004a 3.2x -> 4.2x, the dense 64 MB heap 0x100077 only 1.48x -> 1.64x.
* In run 5 the wire/raw ratio of the six biggest memories fell from 48 % (run 4, LZ4) to 39 % (zstd-1): the same as the offline estimate.
  Server side, decoding stays cheap (4.5 M memory writes, 38.5 s in total); the server is busy roughly a quarter of the time, so it is not the bottleneck.
