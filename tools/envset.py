import json,pathlib,subprocess,sys,time
# Użycie: envset.py KLUCZ=wartość -KLUCZ ... ; czeka na zamknięcie Heroic (nadpisałby konfigurację), potem zapisuje zmiany w konfiguracji Tomb Raidera.
while subprocess.call(["pgrep","-x","heroic"],stdout=subprocess.DEVNULL)==0: time.sleep(1)
time.sleep(1)
args=sys.argv[1:]
gid="f7cc1c999ac146f39b356f53e3489514"
if args and args[0].startswith("game="):
    gid=args.pop(0)[5:]
p=pathlib.Path.home()/f".config/heroic/GamesConfig/{gid}.json"
c=json.loads(p.read_text()); g=c[p.stem]; h={e["key"]:e for e in g["enviromentOptions"]}
for a in args:
    if a.startswith("-"): h.pop(a[1:],None)
    else: k,v=a.split("=",1); h[k]={"key":k,"value":v}
g["enviromentOptions"]=list(h.values()); p.write_text(json.dumps(c,indent=2))
print("zastosowane:",sorted(k for k in h if k!="GPU_ICD_TOKEN_FILE"),flush=True)
