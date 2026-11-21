#!/usr/bin/env bash
# Niezależny przebieg A/B Cyberpunka: ab-run.sh <wynik.txt> <nazwa:KLUCZ=wartość,...> <nazwa:...> ...
# Każdy wariant: do 2 prób (limit 420 s każda, po limicie gra jest ubijana), wynik trafia do pliku bez udziału człowieka ani sesji.
# Przykład: ab-run.sh /tmp/ab.txt "on:GPU_ICD_PRESCAN=1" "off:GPU_ICD_PRESCAN=0"
out=${1:?}; shift
here=$(cd "$(dirname "$0")" && pwd)
: > "$out"
for spec in "$@"; do
  name=${spec%%:*}; kv=${spec#*:}; args=${kv//,/ }
  for try in 1 2; do
    ssh -o BatchMode=yes desktop 'pkill -f "[g]ogdl|[u]mu_run|[R]EDprelauncher|[C]yberpunk2077"; pkill -INT -x heroic; for i in 1 2 3 4 5 6 7 8; do pgrep -x heroic >/dev/null || break; sleep 1; done; pkill -x heroic; sleep 2' >/dev/null 2>&1
    log=/tmp/ab-$name-$try.log
    BENCH_TIMEOUT=420 "$here/cp-bench.sh" "ab-$name" $args >"$log" 2>&1
    if grep -q "FPS" "$log" && ! grep -q "Traceback\|przekroczono" "$log"; then
      { echo "== $name (próba $try)"; sed -n '/== wynik/,$p' "$log" | head -30; } >> "$out"; break
    fi
    echo "== $name próba $try nieudana: $(tail -2 "$log" | tr '\n' ' ' | cut -c1-200)" >> "$out"
  done
done
ssh -o BatchMode=yes desktop 'pkill -f "[g]ogdl|[u]mu_run|[R]EDprelauncher|[C]yberpunk2077"; pkill -INT -x heroic; for i in 1 2 3 4 5 6 7 8; do pgrep -x heroic >/dev/null || break; sleep 1; done; pkill -x heroic' >/dev/null 2>&1
echo KONIEC >> "$out"
