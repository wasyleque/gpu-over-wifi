#!/usr/bin/env bash
# Zbiera z klienta testowego (domyślnie minipc) wyniki ostatniego przebiegu i drukuje zestawienie:
#   FPS z okien logu (średnia, Syria/Geothermal-like okna niskie), CPU wątków (próbnik thrsample), taktowanie i temperaturę (thermlog), rozbicie skanu, bajty na łączu.
# Użycie: tools/run-report.sh [log=icd-rottr.log] [host=wasyl@192.168.1.247]
set -euo pipefail
log=${1:-icd-rottr.log}
host=${2:-wasyl@192.168.1.247}
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
ssh_opts=(-o BatchMode=yes -o ConnectTimeout=8)
[ -n "${GPU_KNOWN_HOSTS:-}" ] && ssh_opts+=(-o "UserKnownHostsFile=$GPU_KNOWN_HOSTS")
for f in "$log" thr.jsonl therm.csv; do
  scp -q "${ssh_opts[@]}" "$host:gpu-hub-client/$f" "$tmp/$f" 2>/dev/null || echo "(brak $f)" >&2
done
[ -s "$tmp/$log" ] && python3 -I "$here/icd-log-summary.py" "$tmp/$log" 2>&1 | sed -n 1,5p
python3 -I - "$tmp" "$log" <<'PY'
import sys, re, json, csv, collections, statistics as st, os
tmp, log = sys.argv[1], sys.argv[2]
t = open(os.path.join(tmp, log), errors="replace").read() if os.path.exists(os.path.join(tmp, log)) else ""
def mx(pat):
    v = [int(x) for x in re.findall(pat, t)]
    return max(v) if v else 0
frames = mx(r"klatek (\d+):")
print(f"\nklatek: {frames}, na łączu: {mx(r'na łączu (\d+) B') / 1e9:.1f} GB")
m = re.findall(r"rozbicie skanu: take_dirty_all (\d+) ms \(w tym ioctl PAGEMAP_SCAN (\d+) ms w (\d+) wywołaniach, (\d+) µs/wywołanie\), przetwarzanie pamięci (\d+) ms w (\d+) wejściach \((\d+) µs/wejście\)", t)
if m:
    a = [int(x) for x in m[-1]]
    print(f"skan: take_dirty_all {a[0]} ms (ioctl {a[1]} ms, {a[2]} wywołań, {a[3]} µs), przetwarzanie pamięci {a[4]} ms ({a[5]} wejść, {a[6]} µs)")
p = os.path.join(tmp, "thr.jsonl")
if os.path.exists(p):
    rows = [json.loads(l) for l in open(p)]
    tot = collections.Counter(); n = max(len(rows), 1)
    for r in rows:
        for k, v in r["cpu"].items():
            tot[k.split(":")[0]] += v
    print(f"\nproces: śr {sum(sum(r['cpu'].values()) for r in rows) / n:.0f}% (100% = 1 rdzeń); najwięksi:", ", ".join(f"{k} {v / n:.0f}%" for k, v in tot.most_common(6)))
p = os.path.join(tmp, "therm.csv")
if os.path.exists(p):
    r = [x for x in csv.DictReader(open(p)) if x["mhz_avg"]]
    busy = [(int(x["mhz_avg"]), int(x["temp_max"])) for x in r if 140 < int(x["t"]) < 320] or [(int(x["mhz_avg"]), int(x["temp_max"])) for x in r]
    if busy:
        print(f"taktowanie w grze: śr {st.mean(b[0] for b in busy):.0f} MHz (min {min(b[0] for b in busy)}), temp. maks. {max(b[1] for b in busy)} °C")
PY
