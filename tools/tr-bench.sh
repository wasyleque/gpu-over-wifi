#!/usr/bin/env bash
# Bezobsługowy przebieg wbudowanego benchmarka Rise of the Tomb Raider na DESKTOPIE (Heroic + Proton + nasz ICD), własny odpowiednik skryptów Windows:
# nic nie kasuje (czyta tylko pliki wyników nowsze niż start przebiegu), nie wysyła niczego do sieci.
# Kroki: zamknij Heroic, ustaw zmienne (envset.py), uruchom grę, po BENCH_MENU_WAIT s przełącz okno na pełny ekran (hyprctl) i wyślij 7x strzałkę w dół + Enter (wtype),
# Klawisze są przytrzymywane (150-300 ms): szybkie naciśnięcie i puszczenie w tej samej chwili gra pomija (sprawdzone zrzutem menu).
# poczekaj na trzy pliki wyników, wypisz wynik i dopisz wiersz do docs/measurements/tr-bench-runs.tsv.
# Użycie: tools/tr-bench.sh <nazwa> [KLUCZ=wartość ...] [-KLUCZ ...]    Zmienne: BENCH_HOST (desktop) BENCH_MENU_WAIT (45) BENCH_TIMEOUT (600) BENCH_KEEP_HEROIC=1
set -euo pipefail
name=${1:?Użycie: tr-bench.sh <nazwa> [KLUCZ=wartość ...] [-KLUCZ ...]}; shift
host=${BENCH_HOST:-desktop}; menu_wait=${BENCH_MENU_WAIT:-45}; timeout=${BENCH_TIMEOUT:-600}
gid=f7cc1c999ac146f39b356f53e3489514
changes="$*"
here=$(cd "$(dirname "$0")" && pwd)
r() { ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" "$@"; }
docs='$HOME/Games/Heroic/Prefixes/Rise of the Tomb Raider 20 Year Celebration/drive_c/users/steamuser/Documents/Rise of the Tomb Raider'
hypr='export XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 XDG_SESSION_TYPE=wayland; export HYPRLAND_INSTANCE_SIGNATURE=$(ls /run/user/1000/hypr | head -1)'

echo "== [$name] przygotowanie"
r 'pgrep -f "[R]OTTR.exe" >/dev/null' && { echo "gra już działa: poczekaj na koniec."; exit 1; } || true
r 'pgrep -x heroic >/dev/null && { pkill -INT -x heroic; for i in 1 2 3 4 5 6 7 8; do pgrep -x heroic >/dev/null || break; sleep 1; done; pkill -x heroic; sleep 2; }; pgrep -x heroic >/dev/null && echo "Heroic nie chce się zamknąć" && exit 1; exit 0'
[ $# -gt 0 ] && r "cd ~/gpu-hub-client && python3 envset.py game=$gid $*" | tail -1 | cut -c1-120 || true
r 'cd ~/gpu-hub-client && mv -f icd-rottr.log icd-rottr.last.log 2>/dev/null; true'
t0=$(r 'date +%s')

echo "== start gry"
r "$hypr; export XDG_SESSION_TYPE=wayland; (setsid nohup heroic --ozone-platform=wayland 'heroic://launch/legendary/$gid' >/tmp/heroic-tr-launch.log 2>&1 </dev/null &)"
s0=$(date +%s)
until r 'pgrep -f "[R]OTTR.exe" >/dev/null'; do
  [ $(( $(date +%s) - s0 )) -gt 240 ] && { echo "gra nie wystartowała w 240 s"; r 'tail -5 /tmp/heroic-tr-launch.log'; exit 2; }; sleep 5
done
echo "gra wystartowała po $(( $(date +%s) - s0 )) s; czekam $menu_wait s na menu"
sleep "$menu_wait"
# pełny ekran Hyprlanda (jak ręczne Super+F) i nawigacja klawiszami do benchmarka
r "$hypr; hyprctl dispatch fullscreen 0 >/dev/null 2>&1; sleep 3; for i in 1 2 3 4 5 6 7; do wtype -P Down -s 250 -p Down; sleep 1.2; done; sleep 2; wtype -P Return -s 600 -p Return"
echo "wysłano 7x Down + Enter; czekam na wyniki"
sleep 60
if [ "$(r "d=\"$docs\"; find \"\$d\" -maxdepth 1 -name '*_DESKTOP_20*.txt' ! -name '*frametimes*' -newermt @$t0 | wc -l")" -eq 0 ]; then
  echo "po 60 s brak wyniku: powtarzam Enter"
  r "$hypr; wtype -P Return -s 600 -p Return"
fi
until [ "$(r "d=\"$docs\"; find \"\$d\" -maxdepth 1 -name '*_DESKTOP_20*.txt' ! -name '*frametimes*' -newermt @$t0 | wc -l")" -ge 3 ]; do
  [ $(( $(date +%s) - s0 )) -gt "$timeout" ] && { echo "przekroczono czas $timeout s"; r 'pkill -f "[R]OTTR.exe"; true'; break; }
  r 'pgrep -f "[R]OTTR.exe" >/dev/null' || { echo "gra zakończyła się bez kompletu wyników"; break; }
  sleep 10
done
sleep 3
[ "${BENCH_KEEP_HEROIC:-0}" = 1 ] || r 'pkill -f "[R]OTTR.exe"; sleep 2; pkill -INT -x heroic; for i in 1 2 3 4 5 6 7 8; do pgrep -x heroic >/dev/null || break; sleep 1; done; pkill -x heroic; true'

echo "== wynik"
out=$(r "python3 - <<PY
import glob, os, re
d = os.path.expandvars(\"$docs\")
rows = []
for n in ('SpineOfTheMountain', 'ProphetsTomb', 'GeothermalValley'):
    fs = sorted(f for f in glob.glob(d + '/' + n + '_DESKTOP_20*.txt') if 'frametimes' not in f and os.path.getmtime(f) >= $t0)
    if not fs:
        rows.append((n, '', '', '', '')); continue
    b = open(fs[-1], 'rb').read()
    t = b.decode('utf-16', errors='ignore') if b[:2] in (b'\xff\xfe', b'\xfe\xff') else b.decode('utf-8', 'ignore')
    g = lambda k: (re.findall(k + r'\D*([0-9.]+)', t) or [''])[0]
    rows.append((n, g('Average FPS'), g('Min FPS'), g('Max FPS'), g('Num Frames')))
for x in rows: print('\t'.join(x))
PY")
echo "$out" | awk -F'\t' '{printf "%-20s średnia %s (min %s, maks %s, klatek %s)\n",$1,$2,$3,$4,$5}'
overall=$(echo "$out" | awk -F'\t' '$2!="" {s+=$2*$5; n+=$5} END {if (n>0) printf "%.2f", s/n}')
echo "Overall (średnia ważona klatkami): $overall"
r "grep -E 'gorące okna|pobrania leniwe: [0-9]|strojenie:|pacing' ~/gpu-hub-client/icd-rottr.log | tail -4 | cut -c1-260"
tsv="$here/../docs/measurements/tr-bench-runs.tsv"
[ -f "$tsv" ] || printf 'data\tnazwa\tzmiany\toverall\tspine\tprophets\tgeothermal\n' > "$tsv"
sp=$(echo "$out" | sed -n 1p | cut -f2); pr=$(echo "$out" | sed -n 2p | cut -f2); ge=$(echo "$out" | sed -n 3p | cut -f2)
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date +%F\ %H:%M)" "$name" "$changes" "$overall" "$sp" "$pr" "$ge" >> "$tsv"
echo "zapisano wiersz w $tsv"
