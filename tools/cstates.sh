#!/usr/bin/env bash
# Eksperyment opóźnień sieci (wymaga root): `sudo bash cstates.sh on|off|status`.
# on: wyłącza głębokie stany C procesora (state2 i wyżej) i włącza busy-poll gniazd; off: przywraca; status: pokazuje.
# Zmiany znikają po restarcie maszyny.
set -e
case "${1:-status}" in
  on)  v=1; bp=50 ;;
  off) v=0; bp=0 ;;
  status) grep -H . /sys/devices/system/cpu/cpu0/cpuidle/state*/disable /sys/devices/system/cpu/cpu0/cpuidle/state*/name 2>/dev/null | tr '\n' ' '; echo; sysctl net.core.busy_poll net.core.busy_read; exit 0 ;;
  *) echo "użycie: sudo bash $0 on|off|status"; exit 2 ;;
esac
for f in /sys/devices/system/cpu/cpu*/cpuidle/state[2-9]/disable; do [ -e "$f" ] && echo $v > "$f"; done
sysctl -w net.core.busy_poll=$bp net.core.busy_read=$bp
echo "gotowe: $1"
