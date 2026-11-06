# Khronos Vulkan-Samples through the remote driver, 2026-11-06

Ryzen 5 4500U laptop -> RTX 3090 Ti, `vulkan_samples sample <name> --headless-surface --stop-after-frame 200..300 --width 1280 --height 720`,
build 0.3.6 + device-group instance extension. Frame 150 of each run was dumped (`GPU_ICD_DUMP_FRAME`) and checked for content.

| Sample | Result | Notes |
|---|---|---|
| `dynamic_rendering` | pass | 99% of the frame differs from the background |
| `dynamic_rendering_local_read` | pass | 99% |
| `descriptor_indexing` | pass | bindless, partially bound arrays |
| `descriptor_buffer_basic` | pass | `VK_EXT_descriptor_buffer` |
| `push_descriptors` | pass | two textured crates (`push_descriptors.png`) |
| `texture_loading` | pass | |
| `compute_nbody` | pass | compute + graphics |
| `synchronization_2` | pass | `VK_KHR_synchronization2` |
| `mesh_shading` | pass | frame has few colours (19), plausible for a flat-shaded mesh; not compared against a local reference |
| `oit_linked_lists` | pass | order-independent transparency, atomics |
| `ray_tracing_basic` | **pass (ray tracing)** | with `GPU_ICD_RAYTRACING=1`: 300 frames, no errors; frame 150 is the expected RGB-gradient triangle drawn by a ray-tracing pipeline (`ray_tracing_basic.png`) |
| `ray_tracing_extended`, `ray_tracing_position_fetch` | **pass (ray tracing)** | with `GPU_ICD_RAYTRACING=1`: 300 frames each, no errors, no watchdog |
| `timestamp_queries` | pass | |
| `hello_triangle_1_3` | sample crashes | SIGSEGV in the sample itself: `HeadlessWindow::create_surface` calls a null function pointer (backtrace under gdb: frame 0 is address 0, frame 1 the sample's `create_surface`); not in the driver, no driver log line precedes it; not investigated further |
| `ray_queries` | **pass (ray tracing)** | with `GPU_ICD_RAYTRACING=1`: 300 frames; frame 150 shows the Sponza hall with ray-query shadows / ambient occlusion (`ray_queries.png`) |
| `buffer_device_address` | **not available** | the instance extension `VK_KHR_device_group_creation` is now advertised, but the sample also needs the device extension `VK_KHR_device_group`, which is still filtered (device groups) |
| `shader_object` | environment | needs the layer `VK_LAYER_KHRONOS_shader_object`, which is not installed on the laptop |

"Pass" means: exit code 0, all requested frames presented, no watchdog report, frame 150 is not blank. Pixel-exactness against a local GPU was not
compared. Every run was short (200-300 frames), so FPS is not reported here.

Next capability gap: **ray tracing** (needs encoding of acceleration-structure build infos, shader binding tables and device addresses) and
`VK_KHR_device_group` for a single-device group.

## Ray tracing (added the same day)

`VK_KHR_acceleration_structure`, `VK_KHR_ray_tracing_pipeline`, `VK_KHR_ray_query` and `VK_KHR_deferred_host_operations` were on the deny list.
What was missing was small: the device-or-host address unions of the acceleration-structure build/copy commands (now carried as 8 raw
bytes; only the device-address variant is meaningful, and the server card does not report `accelerationStructureHostCommands`), the arrays
of pointers `ppBuildRangeInfos` / `ppMaxPrimitiveCounts` (length `pInfos[i].geometryCount`, validated on decode), and return values of
type `VkDeviceSize` / `uint32_t`. Opt-in for now: set `GPU_ICD_RAYTRACING=1`; it becomes the default after a game (Cyberpunk, Quake II RTX) has been
checked.
