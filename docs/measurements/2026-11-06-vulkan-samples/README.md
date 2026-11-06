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
| `ray_tracing_basic`, `ray_queries` | **not available** | `VK_KHR_acceleration_structure`, `VK_KHR_ray_tracing_pipeline`, `VK_KHR_ray_query` and `VK_KHR_deferred_host_operations` are on the explicit deny list of `exts.rs`, so the driver does not advertise them |
| `buffer_device_address` | **not available** | the instance extension `VK_KHR_device_group_creation` is now advertised, but the sample also needs the device extension `VK_KHR_device_group`, which is still filtered (device groups) |
| `shader_object` | environment | needs the layer `VK_LAYER_KHRONOS_shader_object`, which is not installed on the laptop |

"Pass" means: exit code 0, all requested frames presented, no watchdog report, frame 150 is not blank. Pixel-exactness against a local GPU was not
compared. Every run was short (200-300 frames), so FPS is not reported here.

Next capability gap: **ray tracing** (needs encoding of acceleration-structure build infos, shader binding tables and device addresses) and
`VK_KHR_device_group` for a single-device group.
