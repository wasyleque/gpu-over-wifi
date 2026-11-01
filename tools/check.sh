#!/usr/bin/env bash
# Pełna weryfikacja: clippy i testy całego workspace na serwerze (32 wątki), a testy gpu-icd lokalnie.
# gpu-icd ma testy zależne od lokalnego sterownika Mesa (laptop); na serwerze z NVIDIĄ część z nich pada w sterowniku.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
echo "== clippy (serwer)"; "$here/remote-build.sh" clippy --workspace --all-targets 2>&1 | grep -E "^(warning|error)" -A4 || echo "czysto"
echo "== testy poza gpu-icd (serwer)"
"$here/remote-build.sh" test --workspace --exclude gpu-icd --no-fail-fast 2>&1 | grep -E "^error|^test result|panicked" | awk '/^test result/{p+=$4; f+=$6; next} {print} END{print p " ok, " f " błędów"}'
echo "== testy gpu-icd (lokalnie)"; (cd "$here/.." && cargo test -q -p gpu-icd 2>&1 | grep -E "^test result|FAILED|panicked" | head -3)
