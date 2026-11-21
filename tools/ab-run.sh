#!/usr/bin/env bash
# Niezależny przebieg A/B Cyberpunka: ab-run.sh <wynik.txt> <nazwa:KLUCZ=wartość,...> <nazwa:...> ...
# Każdy wariant: do 2 prób (limit 420 s każda, po limicie gra jest ubijana), wynik trafia do pliku bez udziału człowieka ani sesji.
# Przykład: ab-run.sh /tmp/ab.txt "on:GPU_ICD_PRESCAN=1" "off:GPU_ICD_PRESCAN=0"
out=${1:?}; shift
here=$(cd "$(dirname "$0")" && pwd)
: > "$out"
# Zabezpieczenie: nie zaczynamy pomiaru, gdy desktop jest zajęty czymś innym niż gra (pobieranie, kopiowanie, kompilacja zawyżają i zaniżają wyniki).
# Czeka do 20 min na ciszę: ruch na interfejsach innych niż łącze do serwera (enp7s0) < 3 MB/s, zapis na dysk < 15 MB/s, load < 3.
wait_quiet() {
  local waited=0
  while [ $waited -lt 1200 ]; do
    local v
    v=$(ssh -o BatchMode=yes desktop 'a=$(awk "NR>2 && \$1!~/^(lo|enp7s0):/ {r+=\$2+\$10} END{print r+0}" /proc/net/dev); d0=$(awk "{w+=\$10} END{print w+0}" /proc/diskstats); sleep 3; b=$(awk "NR>2 && \$1!~/^(lo|enp7s0):/ {r+=\$2+\$10} END{print r+0}" /proc/net/dev); d1=$(awk "{w+=\$10} END{print w+0}" /proc/diskstats); l=$(cut -d" " -f1 /proc/loadavg); echo $(( (b-a)/3000000 )) $(( (d1-d0)*512/3000000 )) $l' 2>/dev/null)
    set -- $v
    if [ -n "${1:-}" ] && [ "${1}" -lt 3 ] && [ "${2}" -lt 15 ] && awk "BEGIN{exit !(${3:-9} < 3)}"; then
      [ $waited -gt 0 ] && echo "odczekano ${waited} s na ciszę na desktopie" >> "$out"
      return 0
    fi
    waited=$((waited+10)); sleep 7
  done
  echo "UWAGA: desktop zajęty (ruch ${1:-?} MB/s, dysk ${2:-?} MB/s, load ${3:-?}); pomiar mimo to wykonany, wynik niepewny" >> "$out"
}
for spec in "$@"; do
  name=${spec%%:*}; kv=${spec#*:}; args=${kv//,/ }
  for try in 1 2; do
    ssh -o BatchMode=yes desktop 'pkill -f "[g]ogdl|[u]mu_run|[R]EDprelauncher|[C]yberpunk2077"; pkill -INT -x heroic; for i in 1 2 3 4 5 6 7 8; do pgrep -x heroic >/dev/null || break; sleep 1; done; pkill -x heroic; sleep 2' >/dev/null 2>&1
    wait_quiet
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
