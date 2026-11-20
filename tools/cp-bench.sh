#!/usr/bin/env bash
# Bezobsługowy przebieg benchmarka Cyberpunka 2077 na DESKTOPIE (Heroic + Proton + nasz ICD): bez klikania.
# Gra startuje z argumentami `--launcher-skip --intro-skip -benchmark` (ustawianymi w konfiguracji Heroic), sama uruchamia benchmark i zapisuje
# wynik w Documents/.../benchmarkResults/<katalog>/summary.json + frames.csv (średnia/min/maks FPS, czas, liczba klatek, czasy klatek).
# Użycie: tools/cp-bench.sh <nazwa> [KLUCZ=wartość ...] [-KLUCZ ...]   (zmienne środowiskowe gry, jak w envset.py)
# Zmienne: BENCH_HOST (desktop)  BENCH_TIMEOUT (900 s)  BENCH_KEEP_HEROIC=1 (nie zamykaj Heroic po przebiegu)
# Wynik: zestawienie na ekranie + wiersz w docs/measurements/cp-bench-runs.tsv
set -euo pipefail
name=${1:?Użycie: cp-bench.sh <nazwa> [KLUCZ=wartość ...] [-KLUCZ ...]}; shift
host=${BENCH_HOST:-desktop}; timeout=${BENCH_TIMEOUT:-900}
gid=1423049311
changes="$*"
here=$(cd "$(dirname "$0")" && pwd)
r() { ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" "$@"; }
res='/home/wasyll/Games/Heroic/Prefixes/Cyberpunk 2077/drive_c/users/steamuser/Documents/CD Projekt Red/Cyberpunk 2077/benchmarkResults'

echo "== [$name] przygotowanie"
r 'pgrep -f "[C]yberpunk2077" >/dev/null' && { echo "gra już działa: poczekaj na koniec."; exit 1; } || true
r 'pgrep -x heroic >/dev/null && { pkill -x heroic; sleep 4; }; pgrep -x heroic >/dev/null && echo "Heroic nie chce się zamknąć" && exit 1; exit 0'
# argumenty gry + zmienne (Heroic zamknięty, więc konfiguracja nie zostanie nadpisana)
r "python3 - <<'PY'
import json, pathlib
p = pathlib.Path.home() / '.config/heroic/GamesConfig/$gid.json'
c = json.loads(p.read_text()); g = c['$gid']
g['launcherArgs'] = '--launcher-skip --intro-skip -benchmark'
p.write_text(json.dumps(c, indent=2))
PY"
[ $# -gt 0 ] && r "cd ~/gpu-hub-client && python3 envset.py game=$gid $*" | tail -1
before=$(r "ls '$res' 2>/dev/null | sort | tail -1" || true)
r 'cd ~/gpu-hub-client && mv -f icd-cp2077.log icd-cp2077.last.log 2>/dev/null; true'

echo "== start gry"
r 'export XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 XDG_SESSION_TYPE=wayland; export HYPRLAND_INSTANCE_SIGNATURE=$(ls /run/user/1000/hypr | head -1); (setsid nohup heroic --ozone-platform=wayland "heroic://launch/gog/'$gid'" >/tmp/heroic-launch.log 2>&1 </dev/null &)'
t0=$(date +%s)
# `pgrep -x Cyberpunk2077.exe` nie działa (nazwa procesu ma limit 15 znaków): szukamy po pełnej linii poleceń
until r 'pgrep -f "[C]yberpunk2077.exe" >/dev/null'; do
  [ $(( $(date +%s) - t0 )) -gt 240 ] && { echo "gra nie wystartowała w 240 s"; r 'tail -5 /tmp/heroic-launch.log'; exit 2; }; sleep 5
done
echo "gra wystartowała po $(( $(date +%s) - t0 )) s"
if [ "${BENCH_PERF:-0}" = 1 ]; then
  # profil CPU całej gry (wszystkie wątki) w środku benchmarka: perf record 40 s po BENCH_PERF_DELAY s
  sleep "${BENCH_PERF_DELAY:-90}"
  r 'pid=$(pgrep -f "[C]yberpunk2077.exe" | head -1); cd ~/gpu-hub-client && perf record -F 499 -g -p $pid -o perf.data -- sleep 40 >perf.out 2>&1; perf report -i perf.data --no-children --sort comm,dso -g none --stdio 2>/dev/null | head -60 > perf-comm.txt; perf report -i perf.data --no-children --comm GameThread --sort dso,sym -g none --stdio 2>/dev/null | head -60 > perf-game.txt' || echo "perf nie zadziałał"
fi
while r 'pgrep -f "[C]yberpunk2077.exe" >/dev/null'; do
  [ $(( $(date +%s) - t0 )) -gt "$timeout" ] && { echo "przekroczono czas $timeout s"; r 'pkill -f '[C]yberpunk2077.exe''; break; }; sleep 10
done
[ "${BENCH_KEEP_HEROIC:-0}" = 1 ] || r 'pkill -x heroic; true'

echo "== wynik"
dir=$(r "ls '$res' | sort | tail -1")
if [ "$dir" = "$before" ] || [ -z "$dir" ]; then echo "brak nowego wyniku benchmarka (gra zakończyła się bez niego, np. awaria)"; exit 3; fi
out=$(r "python3 -c \"
import json
d=json.load(open('$res/$dir/summary.json'))['Data']
print(d['averageFps'], d['minFps'], d['maxFps'], d['time'], d['frameNumber'], d.get('presetName'))\"")
set -- $out
echo "średnia $1 FPS, min $2, maks $3, czas $4 s, klatek $5, preset $6 (katalog $dir)"
r "grep -E 'czas skanów wg|skany pamięci wg|pobrania leniwe' ~/gpu-hub-client/icd-cp2077.log | tail -3 | cut -c1-260"
tsv="$here/../docs/measurements/cp-bench-runs.tsv"
[ -f "$tsv" ] || printf 'data\tnazwa\tzmiany\tsrednia\tmin\tmaks\tczas\tklatek\n' > "$tsv"
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date +%F\ %H:%M)" "$name" "$changes" "$1" "$2" "$3" "$4" "$5" >> "$tsv"
echo "zapisano wiersz w $tsv"
