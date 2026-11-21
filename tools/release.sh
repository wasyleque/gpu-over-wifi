#!/usr/bin/env bash
# Wypchnięcie wersji: podniesiona wersja -> testy -> tłumaczenia dokumentów (en/pl/es) -> commit -> push. Przerywa przy pierwszym błędzie.
# Użycie: tools/release.sh "opis zmiany"   (wersję w Cargo.toml i Cargo.lock podnosisz wcześniej)
set -euo pipefail
cd "$(dirname "$0")/.."
msg=${1:?Użycie: tools/release.sh "opis zmiany"}
git fetch -q origin
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
old=$(git show origin/main:Cargo.toml | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)
[ "$ver" != "$old" ] || { echo "Wersja $ver bez zmian względem origin/main: podnieś ją (Cargo.toml + Cargo.lock)."; exit 1; }
out=$(cargo test --release --workspace 2>&1) || true
res=$(echo "$out" | grep -E "^test result" | awk '{p+=$4; f+=$6} END {print p+0" "f+0}')
set -- "$msg" $res
if [ "${2:-0}" = 0 ] || [ "${3:-1}" != 0 ]; then
  echo "Testy nie przeszły albo się nie uruchomiły (zaliczone ${2:-0}, nieudane ${3:-?}). Ostatnie linie:"
  echo "$out" | tail -15
  exit 1
fi
python3 tools/translate-docs.py
git add -A
git commit -q -m "Wersja $ver: $msg

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
git push -q origin main
git log --oneline -1
