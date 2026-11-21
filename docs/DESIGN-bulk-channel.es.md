<!-- i18n-switch -->
[English](DESIGN-bulk-channel.en.md) · [Polski](DESIGN-bulk-channel.md) · **Español**
<!-- /i18n-switch -->

# Canal separado para escrituras grandes de memoria (proyecto, no implementado)

Estado: **plan guardado el 2026-11-22, nada de esto está implementado** salvo la instrumentación (histograma del tiempo de consultas, punto 0 más abajo). Pruebas y decisión de implementación tras
mediciones en el segundo juego (Shadow of the Tomb Raider) y en Cyberpunk. Objetivo: fluidez (menos fotogramas > 60 ms al cargar recursos), no FPS promedio.

## 1. Problema

Todo lo que el cliente envía al servidor va por una única conexión TCP en una única cola FIFO (`Out` en `crates/gpu-icd/src/link.rs`, capacidad `MAX_QUEUED_BYTES` = 64 MB,
opción `GPU_ICD_SENDQ_MB`): lotes de comandos (`Cmds`), escrituras de memoria (`MemWrite*`, comprimidas por los hilos `gpu-icd-frameN`) y consultas con respuesta (`Query`, `Sync`).
Una consulta que entra en la cola espera detrás de todo lo que ya está en ella, y en el lado del servidor un solo hilo ejecuta las solicitudes en orden.

Medido (Rise of the Tomb Raider, DX11/DXVK, Ryzen 5 1600X, 2,5 Gb/s, vsync desactivado, 2026-11-21/22):

* en promedio, una solicitud con respuesta espera en la cola 261 µs, y desde la escritura hasta la respuesta 798 µs, es decir, el coste promedio de la cola es pequeño (aprox. 0,4 ms por fotograma);
* cola: los volcados `GPU_ICD_SPIKES=60` muestran en la fase de carga de recursos consultas `vkGetImageMemoryRequirements2` de 32-95 ms, `vkGetDescriptorEXT` de 43-46 ms, `vkCreateImage`
  con espera de 69 ms por el bloqueo de instancia, `vkGetSemaphoreCounterValue` de 71 ms; en las escenas del benchmark 11-12 fotogramas > 60 ms de aprox. 975 (escenas 3 y 2), p99 de fotograma 62-64 ms con p50 de 23-29 ms;
* al saturar el enlace (recorrido con LZ4, 230 MB/s), estos tiempos crecían hasta 100 ms y los fotogramas se congelaban (`hamulec_klatek` 100 ms).

Ganancia esperada: FPS promedio +1-3% (límite superior, porque el coste promedio de la cola es aprox. el 1% del fotograma), mientras que la **fluidez** (cola de la distribución) puede mejorar claramente.

## 2. Idea

Las escrituras grandes de memoria van por una conexión separada (`AttachBulk{session}`, op por asignar), y el flujo de control lleva solo comandos y consultas, de modo que una consulta no espera detrás de
megabytes de escrituras. La coherencia la garantizan números, igual que en el canal de lecturas existente (`AttachReads`, `MemReadAfter`, `ReadView`):

1. **Barrera antes de usar las escrituras.** El cliente numera las escrituras en el canal bulk (u64, sin desbordamiento: el error del número de fotograma de 16 bits, que apareció el 2026-11-21, no puede repetirse).
   Antes del lote de comandos que contiene `vkQueueSubmit*` (y antes de cada solicitud de lectura de memoria), inserta `BulkBarrier{upto}`; el servidor ejecuta la barrera, esperando a que el hilo bulk
   aplique todas las escrituras a `upto`. Esta es exactamente la garantía de orden actual ("escrituras antes del envío") expresada mediante un número en lugar de una posición en un único flujo.
2. **Escritura tras la asignación.** La escritura en el canal bulk lleva `after_ctrl` (número del fotograma de control tras el cual la memoria existe y está mapeada); el hilo bulk espera a `ReadView::wait_for`
   (las mismas funciones y ventana asimétrica que el canal de lecturas) antes de escribir. Sin esto, la escritura podría llegar antes de `vkAllocateMemory`/`vkMapMemory`.
3. **Liberación de memoria.** Antes de `vkFreeMemory`/`vkUnmapMemory`, barrera como en el punto 1 (las escrituras a esa memoria deben terminar).
4. **Espejo (prefijo zstd).** Las escrituras dentro del canal bulk están ordenadas (una sola conexión), por lo que el espejo del cliente y del servidor permanece coherente; las escrituras a LA MISMA memoria nunca
   van simultáneamente por dos vías (el canal bulk sustituye al flujo de control para las escrituras de memoria en su totalidad, no solo para las grandes).
5. **El hilo bulk en el lado del servidor** requiere acceso a los mapeos y al espejo: hoy el espejo vive en `Replayer` (`&mut`, hilo de servicio). Hay que trasladarlo a una estructura compartida
   (`Arc`, bloqueo por memoria) o entregárselo al hilo bulk en exclusiva (entonces las lecturas diferenciales, que hoy usan el espejo, también deben pasar por él). Este es el elemento más difícil
   del proyecto.

## 3. Qué NO resuelve esto

* Consultas que esperan al propio trabajo del servidor (el hilo de servicio ejecuta una solicitud a la vez, replay aprox. 7 ms/fotograma) ni al bloqueo global de instancia en el lado del cliente
  (`vkCreateImage` esperó 69 ms por el bloqueo; sospecha: el productor de escrituras espera espacio en la cola de envío mientras la mantiene, ver más abajo).
* El coste de CPU de la compresión (aprox. 1-1,5 núcleos a 440-500 MB/s de escrituras brutas) ni de la conversión de vídeo.

Conviene comprobar ANTES de construir el canal: si el productor de escrituras espera espacio en la cola (`enqueue_item`, `space.wait`) **manteniendo el bloqueo de instancia** (llamado desde `send_mem` bajo
bloqueo). Si es así, entonces parte de los 30-95 ms es bloqueo, no cola TCP, y es más barato trasladar la espera antes del bloqueo (como se hizo con el freno de fotogramas: `hamulec_klatek` 100 ms
mantenido bajo bloqueo congelaba todos los hilos).

## 4. Plan y orden

0. **Instrumentación (hecho, 0.3.68):** histograma del tiempo de consultas con recorrido por la red (`czas zapytań z obiegiem po sieci: ...` en el log ICD, intervalos <1, <2, <5, <10, <20, <50, <100, >=100 ms
   y p50/p95/p99/máx). Esta es la medida "antes" y "después".
1. **Diagnóstico del bloqueo (medio día):** contador de tiempo `enqueue_item` esperando espacio y si esto ocurre bajo el bloqueo de instancia; en los volcados de saltos (`GPU_ICD_SPIKES`) separar
   "cola TCP" de "bloqueo". El resultado decide si el canal bulk es necesario en absoluto.
2. **Servidor:** hilo bulk con canal `AttachBulk`, barrera `BulkBarrier`, `after_ctrl`; espejo compartido; pruebas con backend simulado (como las pruebas `conn.rs`).
3. **Cliente:** escrituras en el canal bulk, numeración u64, barreras antes de `QueueSubmit*` y de las lecturas, liberación, interruptor `GPU_ICD_BULK=0`, retorno de emergencia al flujo de control
   tras desconectar el canal (tras vaciarlo mediante barrera).
4. **Verificación y carga** (más abajo).

Estimación: 2-4 días, de los cuales la mitad es compartir el espejo y las pruebas de condiciones de carrera.

## 5. Pruebas (requeridas antes de la implementación)

* Unitarias: orden de escrituras en el canal bulk; la barrera espera exactamente hasta `upto`; `after_ctrl` espera a la asignación; desbordamiento de números (u64, sin error de ventana de 16 bits);
  rotura del canal en curso (retorno al flujo de control sin pérdida de escritura).
* Prueba de carrera propuesta en consulta (DeepSeek, 2026-11-21): escritura A (0xAA) y lectura en el segundo canal con dependencia N, inmediatamente escritura B (0xBB) en el principal; la lectura debe
  devolver siempre 0xAA, nunca 0xBB ni mezclas; con retardos aleatorios y muchas repeticiones; prueba de interbloqueo con muchas lecturas simultáneas y flujo intenso.
* Modo de verificación de contenido (análogo a `GPU_ICD_RUNS_VERIFY`): cada N fotogramas el servidor compara el contenido de la memoria GPU con el espejo tras aplicar las escrituras; cualquier divergencia es un error.
* De carga: bucle de arranques del juego (`startloop.sh`, 40 arranques, 0 Xid y 0 salidas del servidor), recorridos de Cyberpunk y Tomb Raider sin errores de orden (divergencias del espejo, Xid 109/32).

## 6. Criterios de éxito y fracaso (medidos antes y después con la misma herramienta)

Punto de referencia (2026-11-22, Tomb Raider, zstd adaptativo): Overall 44,3 FPS; escena 3: p95 46,6 ms, p99 63,6 ms, 12 fotogramas > 60 ms de 975; Siria: p99 61,7 ms, 11 de 980;
histograma de consultas con recorrido por la red, **Cyberpunk 2077** (0.3.68, un recorrido, 31,75 FPS): 45 731 consultas, p50 < 2 ms, p95 < 10 ms, p99 < 50 ms, máx 449,7 ms; intervalos
[<1, <2, <5, <10, <20, <50, <100, >=100 ms] = [15964, 7609, 15745, 4196, 1306, 877, 28, 6]. Es decir, el 2% de las consultas (911 de 45 731) dura >= 20 ms; con una estimación de aprox. 35 ms por tal consulta
y aprox. 3 ms por las restantes, la cola es aprox. 1/4 del tiempo total de consultas. Histogramas de Tomb Raider y Shadow of the Tomb Raider: por recopilar en los próximos recorridos.

Éxito: (a) el número de fotogramas > 60 ms cae al menos un 30%; (b) el p99 del tiempo de consultas cae al menos 2x; (c) Overall no cae más del 1%; (d) 0 divergencias de contenido en el modo
de verificación, 0 Xid en 40 arranques; (e) ninguna salida del canal bulk fuera de lo permitido cambia el orden respecto a los comandos (pruebas de carrera).
Fracaso (retirada, queda la opción `GPU_ICD_BULK=0`): (c) o (d) no cumplidos, o (a) y (b) sin mejora, es decir, prueba de que la cola provenía del bloqueo o del trabajo del servidor, y no de la cola TCP.

## 7. Alternativas más baratas (a comprobar antes de construir el canal)

* Trasladar la espera del productor (`enqueue_item`) antes del bloqueo de instancia (contrapresión antes del bloqueo): cambio pequeño, si el diagnóstico del punto 1 confirma el mecanismo.
* Cola más pequeña (`GPU_ICD_SENDQ_MB=8`) probada el 2026-11-21: sin ganancia (40,8 vs 41,2 FPS), y menos saltos (28 vs 45 fotogramas > 60 ms) — resultado no concluyente con un solo recorrido;
  conviene repetir con el histograma de consultas, porque ahora hay con qué evaluarlas.
