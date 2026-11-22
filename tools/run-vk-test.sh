#!/usr/bin/env bash
# Użycie (na kliencie, po skopiowaniu skryptów): run-vk-test.sh vk-sparse-formats.py | vk-sparse-bind.py  — testy pamięci rzadkiej przez nasz ICD (ctypes).
export VK_DRIVER_FILES=/home/wasyll/gpu-hub-client/gpu_icd_pipelined.json VK_LOADER_DRIVERS_SELECT="*gpu_icd*" GPU_ICD_ADDR=10.42.0.2:7979 GPU_ICD_CODEC=lz4
export GPU_ICD_TOKEN_FILE=$(python3 -c "
import json
for e in json.load(open('/home/wasyll/gpu-hub-client/env-icd-1423049311.json')):
    if isinstance(e,dict) and 'TOKEN' in str(e.get('key','')): print(e.get('value',''))")
timeout 60 python3 -u "$1" 2>&1 | grep -v "^libva\|^\[gpu-icd\]" | head -20
