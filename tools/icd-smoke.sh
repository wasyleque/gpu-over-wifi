#!/usr/bin/env bash
# Test dymny ICD: vulkaninfo na laptopie widzi zdalną kartę.
# Użycie: icd-smoke.sh <adres:port> <plik-tokenu> [oczekiwana-nazwa-karty]
set -euo pipefail
addr=$1; token=$2; want=${3:-RTX 3090 Ti}
root=$(cd "$(dirname "$0")/.." && pwd)
cargo build -q -p gpu-icd --manifest-path "$root/Cargo.toml"
manifest=$(mktemp --suffix=.json)
trap 'rm -f "$manifest"' EXIT
"$root/tools/icd-manifest.sh" "$root/target/debug/libgpu_icd.so" "$manifest" > /dev/null
out=$(GPU_ICD_ADDR=$addr GPU_ICD_TOKEN_FILE=$token VK_DRIVER_FILES=$manifest timeout 60 vulkaninfo --summary 2>&1)
echo "$out" | grep -E "deviceName|apiVersion|deviceType"
echo "$out" | grep -q "deviceName.*$want" && echo "OK: widzę „$want” przez ICD" || { echo "BŁĄD: brak „$want”"; exit 1; }
