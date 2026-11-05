# Black Book (Heroic, Proton-CachyOS, DXVK) on the remote RTX 3090 Ti, 2026-11-05

Ryzen 5 4500U laptop, 1 Gb/s cable, H.264. The screenshot shows the game's own FPS counter at **59.7 FPS**
(`blackbook_60fps.png`).

## What happened during the day

| Step | FPS | Why |
|---|---|---|
| First run | 5 (with every optimisation off) | baseline; GPU hangs (Xid 109) when the optimisations were on |
| Lazy read-back limited to HOST_CACHED memory | ~10 | the hang came from lazy read-back on non-cached memory |
| Lazy read-back for all memory + pending writes sent before a fetch | ~5 | no hang, but ~30 page faults/frame, each a round trip under the instance lock |
| **Server reads non-cached memory through a GPU copy into cached RAM** | **40-57, up to 60** | the CPU read of device-local BAR memory ran at ~380 MB/s (13 ms per 5 MB, 199 s of 283 s of waiting) |

## The finding that mattered

The server spent most of the frame time in `MemReadDelta`, not in commands or the network: 15 173 requests, 199 s,
about 5 MB each. The 4-16 MB dynamic buffers of DXVK live in DEVICE_LOCAL|HOST_VISIBLE memory, which the CPU reads
slowly. The GPU copies that range into a HOST_CACHED buffer at PCIe speed and the server compares pages from there.
Over 14 541 frames: 76 000 GPU copies, 0 failures, `memreaddelta` 51 s (was 199 s over a quarter of the frames),
`vkWaitSemaphores` 26 ms -> 4 ms per call. The data the game sees is the same; any failure falls back to the CPU read.

Switches: `GPU_RENDERD_NO_GPU_COPY=1` (server) disables the GPU copy; `GPU_ICD_LAZY_ALL=1` (client) re-enables
lazy read-back for non-cached memory (slower in this game).

## Later the same night (build 0.3.5)

Compression of memory writes moved off the instance lock and spread over two framing workers (a single framing sender doubled the wait for
the server and dropped the mean to 31 FPS; the pipeline fixed that). 12 113 frames: mean **52.7 FPS**, 60-63 FPS for most of the session; server wait 14.4 ms
per frame, instance lock held 3.0 ms per frame (6.6-7.3 before), `vkWaitSemaphores` 4.4 ms.
