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

## Runs 6 and 7 (Ryzen with 16 GB RAM, zstd)

| run | Mountain Peak | Syria | Geothermal | Overall |
|---|---|---|---|---|
| 6 | 16.06 | 9.62 | 10.01 | **11.55** |
| 7 | 15.98 | 9.69 | 10.00 | **11.55** |

Run 5 (6.6 GB RAM) scored 10.67, so +8 %; runs 6 and 7 agree to 0.01 FPS Overall. RAM was not the main limit.
Texel misses: 3,640 of ~3,700 were format 98 (R32_UINT). Server answers sampled from run 7: `u64 = address >> 2`, `u32 = range >> 2`,
`u32 = 0xe50` (identical in all samples), so `(98, 2, 3664)` was added to `TEXEL_FORMATS` (still gated by learning and audit).

### Run 8 (format 98 synthesised locally)

Mountain Peak 17.57, Syria 9.65, Geothermal 10.00 (10.02), **Overall 11.94**. Texel-buffer misses 3,704 → 94; descriptors answered locally/server 19,864 / 3,533
(run 6: 21,155 / 6,757 at a comparable point). Remaining misses are mostly type 2 (SAMPLED_IMAGE, ~3,240).

### Run 10 (descriptor-buffer memories skipped in post-wait refresh, with runtime audit)

Mountain Peak 19.86, Syria 10.08, Geothermal 10.39, **Overall 12.92** (run 8: 11.94, run 9, transfer-only skip that never triggered: 11.84).
The five 8 MB resource-descriptor heaps (usage 0x420000) and the small sampler heaps (0x220000) are written by the CPU only; skipping them cut server
`memreaddelta` from ~17.6 calls/frame (10.2 ms) to ~8.0 calls/frame (6.0 ms). Audit: 40,124 skips, 627 verified refreshes, 0 misses.
Lesson: the earlier assumption "usage 0x7" came from the memory-properties field, not buffer usage; check `powód odświeżania pamięci` lines before designing around flags.

### Run 11 (eager push of mapped-memory writes at vkEndCommandBuffer, `GPU_ICD_EAGER_PUSH_US=2000`)

Mountain Peak 21.80, Syria 10.42, Geothermal 10.04, **Overall 13.40** (run 10: 12.92, +3.7 %). Wire bytes per frame ~3.29 MB (run 10: ~3.74 MB over a two-run log),
send-queue wait per reply 1.62 ms (run 10: 2.11 ms), server-wait per frame ~44.6 ms (run 10: ~49.2 ms). 8,765 pushes; audit 0 misses of 416. Now the default (2 ms).

### Runs 12-14: noise check (eager push on/off, deferred flush)

| run | change | Mountain Peak | Syria | Geothermal | Overall |
|---|---|---|---|---|---|
| 12 | eager 2 ms + server defers vkFlushMappedMemoryRanges | 19.68 | 10.01 | 9.97 | 12.61 |
| 13 | eager 2 ms, old server | 19.81 | 10.01 | 9.87 | 12.60 |
| 14 | eager **off**, old server | 21.92 | 10.79 | 10.64 | **13.87** |

Eager on: 13.40 / 12.61 / 12.60; eager off: 12.92 / 13.87. Run-to-run spread is therefore at least ~±4 %, much larger than the 0.01 FPS seen once in runs 6/7; the +3.7 % of run 11
was not reproducible. Eager push is now off by default (`GPU_ICD_EAGER_PUSH_US=2000` enables it). Deferred server flush reverted: memwrite time per call 8.7 → 7.5 µs but no FPS gain.

### Runs 18-19: zstd with previous-sent content as prefix (`MemWriteZstdPfx`) and hardware CRC32C

Offline measurement (XORSTAT, 25 GB raw): zstd 9,070 MB, zstd of XOR delta 7,785 MB (-14 %), **zstd with the previous content as prefix 5,248 MB (-42 %)**.
Both sides keep a "last sent here" mirror built from the same write sequence (independent of what the GPU does); the server verifies length and crc32c of the raw bytes.

| run | change | Mountain Peak | Syria | Geothermal | Overall | wire/raw | server memwrite |
|---|---|---|---|---|---|---|---|
| 18 | prefix coding, software CRC32C | 19.67 | 10.40 | 10.76 | 13.18 | 17.8 % | 78.7 s |
| 19 | + hardware CRC32C (SSE4.2) | 22.26 | 12.62 | 14.53 | **16.39** | 19.8 % | 19.1 s |

The byte-at-a-time software CRC32C in `gpu-proto` (~0.5 GB/s) was a hidden cost on both sides (frame CRC of every request and the raw-bytes CRC): replacing it with the SSE4.2 instruction
removed ~60 s of server time per run and cut the client's send-queue wait from 2.2 to 0.77 ms. Overall 13.87 (run 14, best before) → 16.39 (+18 %).

### Run 20 (same configuration as run 19 plus per-memory refresh statistics)

Mountain Peak 26.49, Syria 13.66, Geothermal 15.13, **Overall 18.39** (run 19: 16.39 with the same build apart from the added statistics: identical-configuration spread is therefore at least ±6 %).
Memories still refreshed after the descriptor-buffer skip, GPU writes found: `0x10004a` 11,042 refreshes / 0 with differences, `0x100302` 11,009 / 0, the rest ≤ 663 / 0. In this benchmark the GPU
never wrote any host-visible memory, but this is not provable statically (vkd3d-proton uses device addresses), so refreshing is kept; the cost is the remaining server `memreaddelta` (~19 s).

### Run 21: fragments of one memory merged into one frame (`MemWriteRunsPfx`)

Mountain Peak 26.16, Syria 13.77, Geothermal 14.99, **Overall 18.24** (run 20: 18.39, run 19: 16.39; no measurable change).
Merging: 217,919 frames from 6,433,536 fragments (~30 per frame); server `memwrite` calls 2.7 M → 0.26 M per run, but server time per frame is unchanged (~4.9 ms): the cost is per byte
(decompression, mirror copy, page-hash tracking), not per call. Client send-queue wait per reply 0.77 → 0.48 ms; wire/raw 16.6 % (run 19: 19.8 %).
Per frame (~55 ms): server busy ~18 ms (cmds 7.7, memwrite 4.9, memreaddelta 3.7, present encode 2.0), client waits ~25 ms on the server, ~30 ms is client-side CPU (game + interception). Overlapping the two
(pipelining) is the only large lever left; it needs knowledge of whether the GPU writes host-visible memory (a wait may only return early if no unfetched GPU write can be read).

### Run 22: early return of long `vkWaitSemaphores` (D3D UPLOAD/dynamic-heap contract), deferred replies

Mountain Peak 39.78 (max 88.6), Syria 18.45, Geothermal 19.16, **Overall 26.10** (run 21: 18.24, +43 %; run 5: 10.67, +145 %).
34,325 waits returned early (68,644 deferred replies drained in order), audit of skipped refreshes: 129,357 skips, 2,022 verified, 0 misses; client wait for the server 25 → 5.9 ms per frame
(35.2 s over 5,972 frames); server `query` time rose to 7.4 s (the server now does the waiting, overlapped with the client's recording of the next frame). Engines seen by the ICD: "DXVK" and "vkd3d"; thread names (`dxvk-cs`, `dxvk-queue`) show the game renders through DXVK (D3D11), the vkd3d instance is a probe.

### Run 23 (same build plus per-thread CPU report)

Mountain Peak 37.80 (max 84.9), Syria 17.85, Geothermal 18.74, **Overall 25.08** (run 22: 26.10; spread of identical builds ~±4 %). Early-returned waits: 44,963; audit: 212,927 skips, 3,327 verified, 0 misses.
CPU time per thread name over the whole log (two benchmark passes, 8,988 frames): `ROTTR.exe` x15 147 s, `dxvk-cs` 97 s, `dxvk-queue` 80 s, `gpu-icd-frame` (compression) 2 x 63 s, `gpu-icd-video` 58 s, `gpu-icd-send` 28 s.
No single thread is saturated (each ≤ ~25 % of wall time); the remaining limit is the serial chain game → dxvk-cs → dxvk-queue → interception → server.
