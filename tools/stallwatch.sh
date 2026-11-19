#!/usr/bin/env bash
# Czeka, aż log ICD przestanie rosnąć przy żywej grze, i zdejmuje stosy wszystkich wątków (gdb) do stall-<czas>.txt.
log=${1:-$HOME/gpu-hub-client/icd-cp2077.log}; out=$HOME/gpu-hub-client/stall-$(date +%H%M%S).txt
while :; do
  pid=$(for p in $(pgrep -f '[C]yberpunk2077.exe'); do grep -q libgpu_icd /proc/$p/maps 2>/dev/null && echo $p; done | head -1)
  if [ -n "$pid" ] && [ -f "$log" ]; then
    age=$(( $(date +%s) - $(stat -c %Y "$log") ))
    if [ "$age" -gt 25 ] && [ "$(stat -c %s "$log")" -gt 2000 ]; then
      { echo "pid $pid, log nieaktywny ${age}s"; for t in /proc/$pid/task/*; do echo "$(basename $t) $(cat $t/comm) $(awk '{print $3}' $t/stat) $(cat $t/wchan 2>/dev/null)"; done
        gdb -p $pid -batch -ex "thread apply all bt 25" 2>&1 | grep -vE "^\[New|^warning|Missing|debuginfod"; } > "$out" 2>&1
      echo "zapisano $out"; exit 0
    fi
  fi
  sleep 5
done
