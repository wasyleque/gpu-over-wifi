<!-- i18n-switch -->
[English](CALL-FOR-TESTERS.md) · [Polski](CALL-FOR-TESTERS.pl.md) · **Español**
<!-- /i18n-switch -->

# Ayuda a probar: GPU remota para juegos en máquinas débiles

Buscamos personas con **portátiles, equipos de sobremesa y redes diferentes** que puedan ejecutar un juego a través del controlador Vulkan remoto y enviarnos
los números. Hoy todo se midió en un cliente (un portátil Ryzen 5 4500U) y un servidor (RTX 3090 Ti), así que todavía no sabemos cómo se comporta la pila
en otras CPUs, GPUs, velocidades de enlace, sistemas de ventanas o juegos. Todo informe, incluido "se bloqueó de inmediato", es útil.

## Qué funciona hoy

* Juegos Vulkan nativos de Linux y juegos Proton (DirectX 12 mediante vkd3d-proton, DirectX 11 mediante DXVK) en un **cliente Linux**, renderizados en una GPU NVIDIA de servidor,
  imagen devuelta como H.264 (decodificación por hardware mediante VA-API en el cliente) o LZ4 sin pérdida.
* Medido hasta ahora: FurMark 93 FPS, American Truck Simulator 70 FPS (escenas ligeras, 6 FPS en una escena de cabina concurrida), Hamster Hunter 21-34 FPS,
  Cyberpunk 2077 unos 8 FPS (jugable, 38 FPS en escenas tranquilas). Detalles: [README](../README.md#real-applications-ryzen-5-4500u-laptop---server-a-1-gbs-cable-h264-lab-runs).
* Aún no funciona: clientes Windows, servidores no NVIDIA, juegos solo OpenGL (Zink se bloquea), varios clientes en una GPU a la vez (una sesión a la vez es seguro).

## Qué necesitas

| | Servidor (tiene la GPU potente) | Cliente (la máquina débil) |
|---|---|---|
| SO | Linux | Linux (probado Ubuntu 26.04 y Arch/Omarchy), Wayland o X11 |
| GPU | NVIDIA con Vulkan (controlador 580 probado), usuario en los grupos `render` y `video`; NVENC para H.264 | cualquier GPU con decodificación H.264 VA-API (Intel, AMD) o usar el códec LZ4 |
| Red | un **cable directo de 1 Gb/s** (o mejor) entre las dos máquinas; no se recomienda Wi-Fi (cada fotograma espera ~50 viajes de ida y vuelta) | igual |
| RAM | 8 GB+ | 8 GB mínimo, **16 GB recomendados** (el controlador guarda copias de la memoria mapeada del juego) |
| Compilación | Rust estable reciente (compilamos con 1.99) | Rust estable reciente (compilamos con 1.99) |

El tiempo de ida y vuelta importa más que el ancho de banda: un cable de 0,15 ms dio 3 veces la tasa de fotogramas de un adaptador USB de 0,4 ms en nuestras pruebas.

## Cómo ejecutar una prueba (30 minutos)

1. Sigue [`TESTING-OTHER-MACHINE.md`](TESTING-OTHER-MACHINE.md) para compilar e iniciar el demonio del servidor y el controlador del cliente y para ejecutar `vkcube`.
2. Para un juego, establece las opciones de lanzamiento de Steam (o el entorno) así en el cliente. `VK_LOADER_DRIVERS_SELECT` importa: un valor heredado de Steam puede ocultar el controlador.
   ```
   VK_DRIVER_FILES=$HOME/.local/share/gpu-hub/gpu_icd.json VK_LOADER_DRIVERS_SELECT=*gpu_icd* \
   GPU_ICD_ADDR=<server>:7979 GPU_ICD_TOKEN_FILE=$HOME/.config/gpu-hub/renderd.token \
   GPU_ICD_CODEC=h264 GPU_ICD_STATS=periodic GPU_ICD_LOG=$HOME/gpu-hub-icd.log %command%
   ```
   Para juegos Proton que inician un lanzador, añade el argumento de omitir lanzador del juego (Cyberpunk: `--launcher-skip -skipStartScreen`).
3. Confirma que el juego realmente usa la GPU del servidor: el registro del propio juego o `vulkaninfo --summary` debe listar la GPU del servidor, y `nvidia-smi` en el servidor debe mostrar carga.
4. Juega 5-10 minutos en un lugar repetible (la misma escena cada vez), luego cierra el juego normalmente.
5. Ejecuta `tools/collect-report.sh <server-address> [path-to-icd-log]` y adjunta la salida a una nueva incidencia usando la plantilla **Informe de prueba**.

## Qué contiene el informe (nada secreto)

Hardware y SO del cliente y del servidor, el tiempo de ida y vuelta y la velocidad del enlace, la versión del controlador y las estadísticas propias del controlador: FPS por ventana de 5 s, viajes
de ida y vuelta por comando, contención de bloqueos, volumen de subida de memoria y el informe del vigilante si un hilo se quedó atascado. Sin tokens, sin rutas de archivo más allá del nombre del registro, sin capturas de pantalla.
Cuéntanos también el juego, sus ajustes gráficos y resolución, y cómo se sintió (el número no lo es todo).

## Si algo sale mal

* **Ventana negra o "no responde" durante un minuto al inicio:** espera, la carga es lenta al principio (sombreadores). No mates el juego antes de 2 minutos.
* **El juego sale con un mensaje de vigilante o de bloqueo después de cargar una partida:** adjunta el informe de bloqueo del propio juego y el registro del controlador (las líneas `WATCHDOG:` dicen qué hilo esperó a qué).
* **La memoria de la GPU del servidor sigue ocupada después de un bloqueo:** reinicia el demonio (`pkill gpu-renderd`, inícialo de nuevo).
* **Nada se conecta:** comprueba los permisos del archivo de token y que el servidor escuche en la dirección del cable, no en 0.0.0.0 (consulta la [nota de seguridad](../README.md#security)).

## Licencia y seguridad

El código está disponible en fuente bajo FSL-1.1-ALv2 (gratis para cualquier uso no competidor, Apache-2.0 después de dos años), consulta [`LICENSE`](../LICENSE). El demonio
**no está reforzado para redes no confiables**: usa solo un cable directo o una VPN, y mantén el token privado.
