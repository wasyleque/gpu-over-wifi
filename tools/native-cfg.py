import json,pathlib,subprocess,time,os,sys
h=pathlib.Path.home(); p=h/".config/heroic/GamesConfig/f7cc1c999ac146f39b356f53e3489514.json"
store=h/"gpu-hub-client/tr-env-icd.json"
mode=sys.argv[1]  # native | icd
while subprocess.call(["pgrep","-x","heroic"],stdout=subprocess.DEVNULL)==0: time.sleep(1)
time.sleep(1)
c=json.loads(p.read_text()); g=c[p.stem]
if mode=="native":
    keep=[e for e in g["enviromentOptions"] if not (e["key"].startswith("GPU_ICD") or e["key"] in ("VK_DRIVER_FILES","VK_LOADER_DRIVERS_SELECT","LIBVA_DRIVER_NAME","NVD_BACKEND"))]
    if not store.exists(): store.write_text(json.dumps([e for e in g["enviromentOptions"] if e not in keep]))
    g["enviromentOptions"]=keep
else:
    saved=json.loads(store.read_text()); have={e["key"] for e in g["enviromentOptions"]}
    g["enviromentOptions"]+= [e for e in saved if e["key"] not in have]
p.write_text(json.dumps(c,indent=2)); print(mode,"zastosowane",[e["key"] for e in g["enviromentOptions"]],flush=True)
