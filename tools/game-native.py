import json,pathlib,subprocess,time,sys
# Użycie: game-native.py <id gry> native|icd ; czeka na zamknięcie Heroic, potem zdejmuje (native) albo przywraca (icd) zmienne naszego sterownika.
gid,mode=sys.argv[1],sys.argv[2]
h=pathlib.Path.home(); p=h/f".config/heroic/GamesConfig/{gid}.json"; store=h/f"gpu-hub-client/env-icd-{gid}.json"
while subprocess.call(["pgrep","-x","heroic"],stdout=subprocess.DEVNULL)==0: time.sleep(1)
time.sleep(1)
c=json.loads(p.read_text()); g=c[gid]
if mode=="native":
    drop=lambda e: e["key"].startswith("GPU_ICD") or e["key"] in ("VK_DRIVER_FILES","VK_LOADER_DRIVERS_SELECT","LIBVA_DRIVER_NAME","NVD_BACKEND")
    if not store.exists(): store.write_text(json.dumps([e for e in g["enviromentOptions"] if drop(e)]))
    g["enviromentOptions"]=[e for e in g["enviromentOptions"] if not drop(e)]
else:
    saved=json.loads(store.read_text()); have={e["key"] for e in g["enviromentOptions"]}
    g["enviromentOptions"]+=[e for e in saved if e["key"] not in have]
p.write_text(json.dumps(c,indent=2)); print(mode,"zastosowane",sorted(e["key"] for e in g["enviromentOptions"] if e["key"]!="GPU_ICD_TOKEN_FILE"),flush=True)
