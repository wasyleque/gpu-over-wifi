# nvEncodeAPI.h

Plik pochodzi z projektu `nv-codec-headers` (FFmpeg), tag `n12.2.72.0` (NVENC API 12.2), ścieżka `include/ffnvcodec/nvEncodeAPI.h`.
Nagłówek niesie notę uprawniającą w stylu MIT (Copyright NVIDIA Corporation; zgoda na kopiowanie, modyfikację i rozpowszechnianie przy
zachowaniu noty), którą zachowujemy w pliku bez zmian. Wersja 12.2 jest wybrana celowo: sterownik na serwerze (580.x) obsługuje API do 13.0,
a wersje starsze działają dzięki zgodności wstecz (nagłówek 13.1 byłby za nowy).

Służy do wygenerowania powiązań Rust (`crates/gpu-nvenc/src/sys.rs`) i do testów zgodności układu struktur z kompilatorem C.
