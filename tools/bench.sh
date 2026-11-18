#!/usr/bin/env bash
# Orkiestrator przebiegu testowego na kliencie (domyślnie minipc): ustawia zmienne w konfiguracji gry w Heroic, rotuje logi, uruchamia próbniki
# (wątki, temperatura, opcjonalnie perf), czeka na grę, odczytuje wynik benchmarka z zrzutu ekranu OCR-em (tesseract) i drukuje zestawienie.
#
# Użycie:  tools/bench.sh <nazwa> [KLUCZ=wartość ...] [-KLUCZ ...]     np.  tools/bench.sh lz4-baza GPU_ICD_MEM_LZ4=1 -GPU_ICD_ZSTD_LEVEL
#   KLUCZ=wartość dodaje/zmienia zmienną środowiskową gry, -KLUCZ usuwa ją. Zmiany obowiązują do następnego wywołania (konfiguracja jest zapisywana na stałe).
# Zmienne środowiskowe sterujące skryptem:
#   BENCH_HOST (wasyl@192.168.1.247)  BENCH_GAME (id gry w Heroic, domyślnie Rise of the Tomb Raider)  BENCH_LOG (icd-rottr.log)
#   BENCH_AUTOKEYS  lista kroków klawiatury "nazwa:opóźnienie_s" rozdzielonych spacją, wysyłana przez wtype po starcie gry (np. "Return:20 Down:1 Return:2");
#                   bez niej skrypt tylko czeka na ręczne przejście do benchmarka i wykrywa wynik na ekranie
#   BENCH_PERF=1    włącza zapis perf (130 s po starcie, 40 s)    BENCH_TIMEOUT (1500 s)
# Wynik: zestawienie na ekranie i wiersz w docs/measurements/bench-runs.tsv
set -euo pipefail
name=${1:?Użycie: bench.sh <nazwa> [KLUCZ=wartość ...] [-KLUCZ ...]}; shift
host=${BENCH_HOST:-wasyl@192.168.1.247}
game=${BENCH_GAME:-f7cc1c999ac146f39b356f53e3489514}
log=${BENCH_LOG:-icd-rottr.log}
timeout=${BENCH_TIMEOUT:-1500}
here=$(cd "$(dirname "$0")" && pwd)
opts=(-o BatchMode=yes -o ConnectTimeout=8)
[ -n "${GPU_KNOWN_HOSTS:-}" ] && opts+=(-o "UserKnownHostsFile=$GPU_KNOWN_HOSTS")
r() { ssh "${opts[@]}" "$host" "$@"; }

echo "== [$name] przygotowanie na $host"
if r 'pgrep -x heroic >/dev/null'; then echo "Heroic jest otwarty: zamknij go całkiem (nadpisze konfigurację przy zamykaniu) i uruchom skrypt ponownie."; exit 1; fi
load=$(r 'cut -d" " -f1 /proc/loadavg'); echo "obciążenie systemu: $load"
awk "BEGIN{exit !($load>0.5)}" && { echo "obciążenie > 0,5: poczekaj aż spadnie i uruchom ponownie."; exit 1; } || true

# 1. konfiguracja gry: zmienne środowiskowe
changes=$(printf '%s\n' "$@")
r "BENCH_GAME=$game python3 -I -" <<PY
import json, os, pathlib, shutil, time, sys
game = os.environ["BENCH_GAME"]
p = pathlib.Path.home() / ".config/heroic/GamesConfig" / (game + ".json")
shutil.copy(p, str(p) + ".bak" + str(int(time.time())))
c = json.loads(p.read_text()); g = c[game]
h = {e["key"]: e for e in g["enviromentOptions"]}
for ch in """$changes""".split():
    if ch.startswith("-"): h.pop(ch[1:], None)
    elif "=" in ch:
        k, v = ch.split("=", 1); h[k] = {"key": k, "value": v}
g["enviromentOptions"] = list(h.values()); p.write_text(json.dumps(c, indent=2))
print("zmienne GPU_ICD*:", ", ".join(f"{k}={v['value'] if 'TOKEN' not in k else '<ukryte>'}" for k, v in sorted(h.items()) if k.startswith("GPU_ICD") and k != "GPU_ICD_TOKEN_FILE"))
PY

# 2. logi i próbniki
r "cd ~/gpu-hub-client && mv -f $log \${$log%.log}.last.log 2>/dev/null; mv -f thr.jsonl thr.last.jsonl 2>/dev/null; mv -f therm.csv therm.last.csv 2>/dev/null; rm -f perf.done
  (setsid nohup python3 -I thrsample.py >/dev/null 2>&1 </dev/null &); (setsid nohup ./thermlog.sh >/dev/null 2>&1 </dev/null &)
  $( [ "${BENCH_PERF:-0}" = 1 ] && echo '(setsid nohup ./perfrun.sh 130 40 >perfrun.out 2>&1 </dev/null &)' )"
echo ">> URUCHOM GRĘ Z HEROIC i przejdź do benchmarka (albo ustaw BENCH_AUTOKEYS)."

# 3. czekanie na start gry
t0=$(date +%s)
until r 'pgrep -x ROTTR.exe >/dev/null'; do
  [ $(( $(date +%s) - t0 )) -gt "$timeout" ] && { echo "przekroczono czas oczekiwania na start gry"; exit 2; }; sleep 5
done
echo "gra wystartowała po $(( $(date +%s) - t0 )) s"

# 4. opcjonalna nawigacja klawiszami (wtype wysyła do okna z fokusem)
if [ -n "${BENCH_AUTOKEYS:-}" ]; then
  for step in $BENCH_AUTOKEYS; do
    key=${step%%:*}; wait=${step##*:}; sleep "$wait"
    r "XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 wtype -k $key" || echo "(wtype $key nie powiodło się)"
  done
fi

# 5. wykrywanie wyniku benchmarka na ekranie (OCR zrzutu co 10 s) albo zakończenie gry
res=""
while r 'pgrep -x ROTTR.exe >/dev/null'; do
  [ $(( $(date +%s) - t0 )) -gt "$timeout" ] && { echo "przekroczono czas przebiegu"; break; }
  r 'XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 HYPRLAND_INSTANCE_SIGNATURE=$(ls /run/user/1000/hypr | head -1) grim /tmp/bench-shot.png 2>/dev/null' || true
  scp -q "${opts[@]}" "$host:/tmp/bench-shot.png" /tmp/bench-shot.png 2>/dev/null || true
  if [ -s /tmp/bench-shot.png ]; then
    txt=$(tesseract /tmp/bench-shot.png - --psm 6 2>/dev/null || true)
    if grep -q "Overall score" <<<"$txt"; then res=$(grep -E "Mountain Peak|Syria|Geothermal|Overall" <<<"$txt"); echo "wynik odczytany z ekranu:"; echo "$res"; break; fi
  fi
  sleep 10
done
# zrzut ze screenshotów użytkownika jako zapas (OCR najnowszego)
if [ -z "$res" ]; then
  f=$(r 'ls -t ~/Pictures/Screenshots/*.png 2>/dev/null | head -1' || true)
  if [ -n "$f" ]; then scp -q "${opts[@]}" "$host:$f" /tmp/bench-shot2.png && res=$(tesseract /tmp/bench-shot2.png - --psm 6 2>/dev/null | grep -E "Mountain Peak|Syria|Geothermal|Overall" || true); [ -n "$res" ] && { echo "wynik z najnowszego zrzutu użytkownika:"; echo "$res"; }; fi
fi

# 6. zestawienie z logów i zapis wiersza
sleep 3
echo "== zestawienie"
GPU_KNOWN_HOSTS=${GPU_KNOWN_HOSTS:-} "$here/run-report.sh" "$log" "$host" 2>&1 | sed -n 1,30p || true
tsv="$here/../docs/measurements/bench-runs.tsv"
[ -f "$tsv" ] || printf 'data\tnazwa\tzmiany\tmountain\tsyria\tgeothermal\toverall\n' > "$tsv"
num() { grep -oE "$1[^0-9]*([0-9]+\.[0-9]+)" <<<"$res" | grep -oE "[0-9]+\.[0-9]+" | head -1; }
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date +%F\ %H:%M)" "$name" "$*" "$(num 'Mountain')" "$(num 'Syria')" "$(num 'Geothermal')" "$(num 'Overall')" >> "$tsv"
echo "zapisano wiersz w $tsv"
