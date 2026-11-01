#!/usr/bin/env bash
# Zlecenie implementacji lokalnemu modelowi na .11 przez aider (jeden plik do edycji, reszta tylko do odczytu).
# Użycie: delegate-aider.sh <plik-zlecenia.txt> <plik-do-edycji> [plik-tylko-do-odczytu ...]
# Model widzi wyłącznie wskazane pliki; bez git, bez auto-commitów; wynik czytam i testuję sam.
set -euo pipefail
[ $# -ge 2 ] || { echo "Użycie: $0 <zlecenie.txt> <plik> [read-only ...]" >&2; exit 2; }
task=$1; target=$2; shift 2
read_args=(); for f in "$@"; do read_args+=(--read "$f"); done
T11=$(ssh -o BatchMode=yes -o ConnectTimeout=8 192.168.0.11 'cat ~/gpu-hub/token' < /dev/null)
OPENAI_API_BASE=http://192.168.0.11:7878/v1 OPENAI_API_KEY="$T11" exec timeout 2400 aider \
  --model openai/qwen3-coder:30b --edit-format whole --no-git --yes-always --no-auto-commits \
  --no-show-model-warnings --no-check-update --timeout 1500 "${read_args[@]}" \
  --message "$(cat "$task")" "$target"
