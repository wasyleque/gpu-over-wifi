#!/usr/bin/env bash
# Tworzy manifest ICD dla loadera Vulkana: icd-manifest.sh <biblioteka.so> <manifest.json>
set -euo pipefail
[ $# -eq 2 ] || { echo "Użycie: $0 <biblioteka.so> <manifest.json>" >&2; exit 2; }
lib=$(realpath "$1")
printf '{\n  "file_format_version": "1.0",\n  "ICD": {\n    "library_path": "%s",\n    "api_version": "1.3.0"\n  }\n}\n' "$lib" > "$2"
echo "zapisano $2 (biblioteka: $lib)"
