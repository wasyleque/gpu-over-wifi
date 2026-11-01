#!/usr/bin/env bash
# Generuje typy i stałe VA-API (tylko typy/stałe, funkcje ładujemy przez dlopen): crates/gpu-vaapi/src/sys.rs.
# Wymaga: bindgen oraz nagłówków libva (/usr/include/va, licencja MIT).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
printf '#include <va/va.h>\n#include <va/va_drm.h>\n' > /tmp/gen-vaapi-$$.h
bindgen /tmp/gen-vaapi-$$.h --allowlist-type 'VAPictureH264|VAPictureParameterBufferH264|VAIQMatrixBufferH264|VASliceParameterBufferH264|VAImage|VAImageFormat|VAConfigAttrib|VAStatus|VABufferType|VAProfile|VAEntrypoint|VAConfigAttribType' --allowlist-var 'VA_STATUS_SUCCESS|VA_RT_FORMAT_YUV420|VA_PICTURE_H264_.*|VA_INVALID_.*|VA_FOURCC_NV12|VA_SLICE_DATA_FLAG_ALL|VA_PROGRESSIVE|VAProfileH264.*|VAEntrypointVLD|VAConfigAttribRTFormat|VA(PictureParameter|IQMatrix|SliceParameter|SliceData)BufferType' --no-layout-tests --no-doc-comments \
  --with-derive-default --rust-target 1.80 --default-enum-style consts --no-prepend-enum-name --use-core \
  --blocklist-function '.*' -- -x c > "$root/crates/gpu-vaapi/src/sys.rs"
rm -f /tmp/gen-vaapi-$$.h
echo "sys.rs: $(wc -l < "$root/crates/gpu-vaapi/src/sys.rs") linii"
