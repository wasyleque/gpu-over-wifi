#!/usr/bin/env bash
# Buduje na serwerze z 32 wątkami (domyślnie 10.42.0.2) zamiast na laptopie z 4: `tools/remote-build.sh <polecenie cargo...>`,
# np. `tools/remote-build.sh build --release -p gpu-renderd` albo `tools/remote-build.sh test --workspace`.
# Źródła są synchronizowane rsync-iem (bez target/ i .git), katalog target zostaje na serwerze między uruchomieniami.
# Nie uruchamiać podczas pomiarów wydajności gry na tym serwerze: kompilacja zajmuje jego procesor.
set -euo pipefail
host=${GPU_BUILD_HOST:-wasyl@10.42.0.2}
dir=${GPU_BUILD_DIR:-gpu-hub-build}
[ $# -ge 1 ] || { echo "Użycie: $0 <polecenie cargo...>" >&2; exit 2; }
root=$(cd "$(dirname "$0")/.." && pwd)
rsync -az --delete --exclude target --exclude .git --exclude '*.mp4' --exclude '*.png' -e "ssh -o BatchMode=yes" "$root/" "$host:$dir/" </dev/null
ssh -o BatchMode=yes "$host" "cd $dir && PATH=\$HOME/.cargo/bin:\$PATH cargo $*" </dev/null
