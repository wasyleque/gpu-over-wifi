<!-- i18n-switch -->
[English](TESTING-OTHER-MACHINE.en.md) · [Polski](TESTING-OTHER-MACHINE.md) · **Español**
<!-- /i18n-switch -->

# Prueba en otro equipo (Linux; Windows todavía no)

Estado a 2026-11-02. **Linux como cliente funciona** (medido en un portátil con Intel HD 520 + RTX 3090 Ti remota). **Windows todavía no está soportado**: requiere un ICD separado para Windows (etapa M8: otro nombre de biblioteca y manifiesto en el registro, WSI Win32, DXGI). El servidor (`gpu-renderd`) funciona en Linux con una tarjeta NVIDIA, AMD o Intel con controlador Vulkan.

## 1. Servidor (máquina con la tarjeta, Linux)
```
cargo build --release -p gpu-renderd          # albo cargo zigbuild --target x86_64-unknown-linux-gnu.2.39 dla starszego glibc
head -c 24 /dev/urandom | base64 > renderd-token && chmod 600 renderd-token
./target/release/gpu-renderd --token-file renderd-token --listen <ADRES-KABLA>:7979
```
Por defecto escucha solo en 127.0.0.1; la dirección de red se indica explícitamente (preferiblemente solo la interfaz de cable, no 0.0.0.0). El usuario del servidor debe pertenecer a los grupos `render` y `video`. En el log del demonio, en cada instancia se ve qué dispositivos detecta (`widzi N urządzeń: [...]`); si solo se ve `llvmpipe`, el controlador de GPU no funciona para ese proceso (reiniciar el demonio ayuda).

## 2. Cliente (máquina donde debe ejecutarse el juego, Linux)
```
cargo build --release -p gpu-icd -p gpu-renderd
tools/icd-manifest.sh target/release/libgpu_icd.so ~/.local/share/gpu-hub/gpu_icd.json
scp serwer:renderd-token ~/.config/gpu-hub/renderd.token && chmod 600 ~/.config/gpu-hub/renderd.token
export GPU_ICD_ADDR=<ADRES-SERWERA>:7979 GPU_ICD_TOKEN_FILE=~/.config/gpu-hub/renderd.token
export VK_DRIVER_FILES=~/.local/share/gpu-hub/gpu_icd.json      # tylko nasz ICD jest widoczny dla aplikacji
```
La tarjeta local para MOSTRAR se detecta automáticamente (orden: Intel, AMD, NVIDIA, otras; se omiten lvp/virtio/gfxstream). Anulación: `GPU_ICD_LOCAL_ICD=/usr/share/vulkan/icd.d/<manifest>.json`.

## 3. Pruebas en orden (de la más simple)
1. `target/release/gpu-probe <ADRES-SERWERA>:7979 --token-file <token>`: lista de tarjetas del servidor y tiempo de consulta.
2. `tools/icd-smoke.sh <ADRES>:7979 <token> "<fragment nazwy karty>"`: `vulkaninfo --summary` ve la tarjeta remota.
3. `target/release/gpu-smoke 20`: búfer e imagen en la tarjeta remota, lectura. `gpu-smoke surface` y `gpu-smoke swapchain`: superficie y cadena de intercambio en superficie headless (sin ventana).
4. `GPU_ICD_STATS=1 vkcube --wsi wayland --c 600 --present_mode 0` (o `--wsi xcb`): cubo en la tarjeta remota. Captura del primer fotograma: `GPU_ICD_DUMP_FRAME=/tmp/klatka.ppm`.

## 4. Opciones
- `GPU_ICD_CODEC=auto|raw|lz4` (hoy disponibles raw y lz4; `h264` y `hevc` están en el protocolo y esperan NVENC y decodificador; elegir un códec no disponible da una advertencia y lz4).
- `GPU_ICD_SYNC_PRESENT=1`: modo síncrono (sin conexión de vídeo separada), para diagnóstico.
- `GPU_ICD_ALL_DEVICES=1`: mostrar también los dispositivos CPU del servidor (llvmpipe).
- `GPU_ICD_STATS=1`: estadísticas en stderr al cerrar; `GPU_ICD_DEBUG=1`, `GPU_ICD_DEBUG_LOCAL=1`: diagnóstico.

## 5. Qué enviarme después de la prueba
- Líneas `gpu-icd statystyki: ...` y `gpu-icd wideo: ...` (bytes por fotograma, tiempos de etapas, recorridos por la red).
- Modelo de la tarjeta local (`vulkaninfo --summary`), versión de Mesa/controlador, compositor (Wayland/X11), enlace (cable/Wi-Fi) y su RTT (`ping`).
- Resultado de `vainfo` (para elegir códec H.264/HEVC en el lado del cliente).
- Si la ventana muestra el cubo correctamente; si no, mensaje de error y `GPU_ICD_DEBUG=1`.

## 6. Limitaciones conocidas
- La imagen vuelve como H.264 en una conexión separada (decodificación VA-API en el cliente); la limitación son los recorridos por la red y el trabajo del cliente, no el servidor (estado y plan: `docs/consult/11-next-steps.md`).
- Los juegos funcionan (FurMark, ATS, Hamster Hunter, Cyberpunk 2077 mediante Proton; resultados en README), pero solo en un cliente; pruebas en otras máquinas: [`CALL-FOR-TESTERS.md`](CALL-FOR-TESTERS.md). Muchas extensiones de Vulkan no se anuncian (regla: solo lo que funciona).
- El demonio en un servidor ha sido inestable al enumerar la tarjeta NVIDIA después de un tiempo prolongado (causa sin explicar; véase AGENTS.md).
- Un cliente autenticado puede enviar comandos Vulkan incorrectos y colgar el controlador del servidor: usa token y una red de confianza.
