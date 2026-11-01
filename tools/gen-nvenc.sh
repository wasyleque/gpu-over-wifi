#!/usr/bin/env bash
# Generuje powiązania NVENC: crates/gpu-nvenc/src/sys.rs (bindgen z third_party/nvenc/nvEncodeAPI.h) oraz consts.rs
# (wartości makr *_VER i stałych GUID wyliczone przez kompilator C). Wymaga: bindgen, gcc.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
hdr="$root/third_party/nvenc/nvEncodeAPI.h"
out="$root/crates/gpu-nvenc/src"
bindgen "$hdr" --allowlist-type 'NV_ENC.*|NV_ENCODE_API_FUNCTION_LIST|GUID|NVENCSTATUS|_NV_ENC.*|_GUID' \
  --allowlist-var 'NVENCAPI.*|NV_ENC.*|NVENC.*' --blocklist-item 'NV_ENC_.*_GUID' --no-layout-tests --no-doc-comments --with-derive-default --with-derive-eq \
  --rust-target 1.80 --default-enum-style consts --no-prepend-enum-name --use-core -- -x c > "$out/sys.rs"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
vers=$(grep -oE '#define (NV_ENC[A-Z0-9_]*_VER|NV_ENCODE_API_FUNCTION_LIST_VER)\b' "$hdr" | awk '{print $2}' | sort -u)
guids=$(grep -oE 'static const GUID NV_ENC_[A-Z0-9_]+_GUID' "$hdr" | awk '{print $4}' | sort -u)
{
  echo '#include <stdio.h>'; echo "#include \"$hdr\""; echo 'int main(void) {'
  for v in $vers; do echo "  printf(\"pub const $v: u32 = %u;\\n\", (unsigned)($v));"; done
  for g in $guids; do
    echo "  printf(\"pub const $g: Guid = Guid { Data1: 0x%08x, Data2: 0x%04x, Data3: 0x%04x, Data4: [0x%02x, 0x%02x, 0x%02x, 0x%02x, 0x%02x, 0x%02x, 0x%02x, 0x%02x] };\\n\", $g.Data1, $g.Data2, $g.Data3, $g.Data4[0], $g.Data4[1], $g.Data4[2], $g.Data4[3], $g.Data4[4], $g.Data4[5], $g.Data4[6], $g.Data4[7]);"
  done
  echo '  return 0; }'
} > "$tmp/c.c"
gcc -std=gnu11 -w "$tmp/c.c" -o "$tmp/c"
{
  echo '//! GENEROWANY przez `tools/gen-nvenc.sh` (wartości policzone kompilatorem C z nagłówka). Nie edytować ręcznie.'
  echo '#![allow(non_upper_case_globals)]'; echo 'use crate::sys::GUID as Guid;'; echo
  "$tmp/c"
} > "$out/consts.rs"
echo "sys.rs: $(wc -l < "$out/sys.rs") linii, consts.rs: $(grep -c '^pub const' "$out/consts.rs") stałych"
