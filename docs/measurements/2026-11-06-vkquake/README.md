# vkQuake (shareware, native Vulkan) on the remote RTX 3090 Ti, 2026-11-06

"Can it run Doom?" Not Doom: Quake (vkQuake 1.37.1, the id Software shareware episode), but yes. Ryzen 5 4500U laptop, 1 Gb/s cable,
H.264, window 1280x720; the screenshot (`vkquake.png`) was taken by the tester during play.

Automated run (built-in `timedemo`, 6 237 frames, `Device: NVIDIA GeForce RTX 3090 Ti`):

| | |
|---|---|
| mean FPS | **54.2** (34-77, steady 50-58) |
| watchdog reports / errors | 0 / none |
| instance lock held | 5.3 s over 561 000 entries (0.85 ms per frame) |
| wait for the server | 9.1 ms per frame, almost all of it `vkWaitForFences` (8.8 ms) |

About 90 `vkBeginCommandBuffer`/`vkEndCommandBuffer` pairs per frame: a good test of many tiny calls.
Game data: `pak0.pak` of the Quake 1.06 shareware (18 689 235 bytes, md5 5906e5998fc3d896ddaf5e6a62e03abb), freely distributable
shareware; it is not part of this repository.
