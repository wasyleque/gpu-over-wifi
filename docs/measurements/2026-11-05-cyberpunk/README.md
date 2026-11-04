# Cyberpunk 2077 on a remote RTX 3090 Ti: the first playable run (2026-11-05)

Cyberpunk 2077 (DirectX 12 through Proton Experimental and vkd3d-proton, `VK_EXT_descriptor_buffer`, 1080p window) on the Ryzen 5 4500U laptop
(6.6 GB RAM, built-in 1 Gb/s Ethernet), rendered by the RTX 3090 Ti of the server, H.264 back. Loading a save and driving in Night City.

**Tester's verdict: playable** ("wow, Cyberpunk is playable"). Last week it was a slideshow at 2-4 FPS and crashed within minutes.

## Numbers (run of 865 s, 7 586 frames, `run-playable-fps.csv`)

| | |
|---|---|
| FPS, 5 s windows | median 7.6, p10 7.2, p90 10.6, up to 38.8 in quiet moments (menus, standing still) |
| Earlier today with the previous driver version | 4-5 FPS (2-4 before the descriptor and memory work) |
| Frame latency (hand-off to display) | 15-20 ms on average, max 34 ms |
| Round trips per frame | 76 (43 of them `vkWaitSemaphores`, 625 us each) |
| Memory uploaded per frame | 5.4 MB raw, 2.3 MB on the wire after LZ4 |
| Lazy read-back of host-cached memory | 8.8 GB fetched on demand in 136 k page-fault batches (1.2 MB/frame), instead of refreshing 28 MB after every GPU wait |
| Commands per frame | ~8 500 on the lock-free recording path, 2 100 locally answered descriptor queries |
| Instance lock, summed over threads | 210 ms/frame waiting, 74 ms/frame held (this is the next bottleneck) |
| Server | executes the whole frame in ~12 ms (decode + commands + encode); GPU 0-25 % busy |

The flat ~7 FPS for minutes points at a fixed per-frame chain of waits, not at the GPU or the link (the link carries 2.3 MB/frame, 160 Mb/s on average).

## What made the difference since the 4-5 FPS version

1. Descriptor-buffer descriptors for buffers are computed locally (verified against the server by periodic audit): -98 % round trips for them.
2. Memory refresh after a GPU wait replaced by lazy read-back (memfd double mapping + `userfaultfd`): the game only pays for pages it reads.
3. Memory scan once per batch instead of once per submit; command recording without the global lock; asynchronous sender thread.
4. A server handle-registry limit and a stale-delta ordering bug (responses applied out of order after the lock was released while waiting) fixed.

## A crash that looked like a freeze

Two start-ups and one session "froze" with a black window. The game wrote `Watchdog timeout! (120 seconds)` (`engineWatchdog.cpp`) to its crash
report: one engine thread was stuck for two minutes while loading the save. The driver now has a watchdog of its own that reports any thread stuck in
one phase for more than 8 s (what it waits for, which command), and an experimental switch (`GPU_ICD_SLICE_WAIT=1`) that sends long semaphore/fence
waits to the server in 1-4 ms slices, because the server executes requests one after another and a long wait for work that another thread has not
submitted yet blocks the whole stream (one lock hold of 16 s was caught).

## Files

* `run-playable.log`: client statistics (5 s samples, round trips per command, lock statistics, upload ranking per memory object, watchdog report).
* `run-playable-fps.csv`: FPS series.
