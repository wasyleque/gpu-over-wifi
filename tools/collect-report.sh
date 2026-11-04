#!/usr/bin/env bash
# Zbiera dane do raportu z testu (bez tokenów i bez zmiennych środowiska): sprzęt, system, łącze, wersja sterownika, statystyki z logu ICD.
# Użycie: tools/collect-report.sh [adres-serwera[:port]] [plik-logu-icd]
set -u
addr=${1:-}; log=${2:-${GPU_ICD_LOG:-}}
host=${addr%%:*}
echo "== zebrano: $(date -Is)"
echo "== sterownik: $(git -C "$(dirname "$0")/.." describe --always --dirty 2>/dev/null || echo nieznana), $(grep -m1 '^version' "$(dirname "$0")/../Cargo.toml" 2>/dev/null)"
echo "== system: $(. /etc/os-release 2>/dev/null; echo "${PRETTY_NAME:-?}"), kernel $(uname -r)"
echo "== procesor: $(lscpu 2>/dev/null | sed -n 's/^Model name:[[:space:]]*//p' | head -1), wątków $(nproc)"
echo "== RAM: $(free -h 2>/dev/null | awk '/^Mem:/{print $2" razem, "$7" dostępne"}'), swap $(free -h 2>/dev/null | awk '/^Swap:/{print $2}')"
echo "== GPU i sieć (lspci):"; lspci 2>/dev/null | grep -iE 'vga|3d|display|ethernet|network' | sed 's/^/   /'
echo "== sesja: ${XDG_SESSION_TYPE:-?}, kompozytor/pulpit: ${XDG_CURRENT_DESKTOP:-?}"
if command -v vulkaninfo >/dev/null; then echo "== vulkaninfo --summary (urządzenia):"; vulkaninfo --summary 2>/dev/null | grep -E 'deviceName|driverName|driverInfo|apiVersion' | sed 's/^/   /' | head -16; fi
if command -v vainfo >/dev/null; then echo "== vainfo (profile H.264):"; vainfo 2>/dev/null | grep -iE 'H264|driver version' | head -5 | sed 's/^/   /'; fi
if [ -n "$host" ]; then
  echo "== ping do $host (20 próbek):"; ping -c 20 -i 0.05 -q "$host" 2>/dev/null | tail -2 | sed 's/^/   /'
  ifc=$(ip route get "$host" 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p' | head -1)
  if [ -n "$ifc" ]; then echo "== interfejs do serwera: $ifc"; ethtool "$ifc" 2>/dev/null | grep -E 'Speed|Duplex' | sed 's/^/   /'; ethtool -i "$ifc" 2>/dev/null | head -1 | sed 's/^/   /'; fi
fi
if [ -n "$log" ] && [ -f "$log" ]; then
  echo "== log sterownika: $log ($(wc -l < "$log") linii)"
  python3 - "$log" <<'PY'
import re, sys
cur = []
last = None
for l in open(sys.argv[1], errors="replace"):
    m = re.search(r"statystyki.*klatek (\d+):", l)
    if m:
        n = int(m.group(1))
        if cur and n < cur[-1]:
            cur = []
        cur.append(n)
        last = l
fps = [(cur[i] - cur[i - 1]) / 5 for i in range(1, len(cur))]
if fps:
    s = sorted(fps)
    print(f"   klatek {cur[-1]}, czas ok. {len(cur) * 5} s, FPS: mediana {s[len(s)//2]:.1f}, p10 {s[len(s)//10]:.1f}, p90 {s[9*len(s)//10]:.1f}, max {max(fps):.1f}")
    print("   FPS (okna 5 s, ostatnie 40):", [round(x) for x in fps[-40:]])
keys = ("obiegi wg poleceń", "blokada instancji", "odczyt leniwy", "wysyłki pamięci wg obiektów", "opóźnienie klatki", "WATCHDOG", "wątek ", "gpu-icd wideo")
seen = {}
for l in open(sys.argv[1], errors="replace"):
    for k in keys:
        if k in l:
            seen[k] = l.rstrip()
for k, v in seen.items():
    print("  ", v[:400])
PY
else
  echo "== brak logu sterownika (ustaw GPU_ICD_LOG i GPU_ICD_STATS=periodic przy teście)"
fi
