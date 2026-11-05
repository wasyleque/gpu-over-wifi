#!/usr/bin/env python3
"""Kopiuje zmienne środowiskowe z globalnych ustawień Heroica do konfiguracji jednej gry (ustawienia gry ZASTĘPUJĄ globalne, nie łączą się z nimi).

Użycie na laptopie klienta, przy ZAMKNIĘTYM Heroicu (Heroic nadpisuje konfigurację przy zamykaniu):
    heroic-apply-env.py <id gry = nazwa pliku w ~/.config/heroic/GamesConfig bez .json> <nazwa logu, np. icd-rottr.log>
Istniejące zmienne gry zostają (poza GPU_ICD_LOG, które dostaje podaną nazwę), brakujące są dopisywane. Przed zmianą powstaje kopia .bak.
"""
import json, shutil, sys
from pathlib import Path

root = Path.home() / ".config/heroic"
game_id, log = sys.argv[1], sys.argv[2]
glob = json.loads((root / "config.json").read_text())["defaultSettings"]["enviromentOptions"]
path = root / "GamesConfig" / f"{game_id}.json"
cfg = json.loads(path.read_text())
sec = cfg[next(k for k in cfg if isinstance(cfg[k], dict) and "enviromentOptions" in cfg[k])]
have = {e["key"]: e for e in sec["enviromentOptions"]}
for e in glob:
    have.setdefault(e["key"], dict(e))
have["GPU_ICD_LOG"] = {"key": "GPU_ICD_LOG", "value": str(Path.home() / "gpu-hub-client" / log)}
sec["enviromentOptions"] = list(have.values())
shutil.copy(path, str(path) + ".bak")
path.write_text(json.dumps(cfg, indent=2))
print("zmienne:", ", ".join(have))
