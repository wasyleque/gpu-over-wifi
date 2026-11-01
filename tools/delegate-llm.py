#!/usr/bin/env python3
"""Zlecenie implementacji lokalnemu modelowi na .11 (natywne API Ollamy): wypełnia todo!() w pliku z czerwonymi testami.

Użycie: delegate-llm.py <zlecenie.txt> <plik.rs> [plik-tylko-do-odczytu ...] [--model NAZWA]
Wynik trafia do <plik.rs>.llm (NIE nadpisuje pliku): skrypt sprawdza, że moduł testów jest identyczny, liczy
zakazane konstrukcje (unwrap/expect/panic poza testami) i drukuje statystyki. Przegląd i kopiowanie robi człowiek.

Dlaczego natywne API: `think:false` wyłącza rozumowanie (inaczej zjada budżet tokenów), a `num_ctx` jest ograniczony,
żeby model w całości mieścił się w VRAM (domyślny, ogromny kontekst przelewał warstwy na CPU: 11 zamiast 89 tok/s).
"""
import json, re, sys, time, urllib.request

HOST = "http://192.168.0.11:11434"
DEFAULT_MODEL = "mgraffam/qwen3.8-coder390:27b"
NUM_CTX = 24576

args = sys.argv[1:]
model = DEFAULT_MODEL
if "--model" in args:
    i = args.index("--model"); model = args[i + 1]; del args[i:i + 2]
if len(args) < 2:
    sys.exit(__doc__)
task, target, readonly = args[0], args[1], args[2:]
src = open(target).read()
marker = "#[cfg(test)]\nmod tests"
prompt = open(task).read() + "\nReturn ONLY the complete file in one ```rust block, nothing else.\n"
for r in readonly:
    prompt += f"\n=== {r} (read-only context) ===\n```rust\n{open(r).read()}```\n"
prompt += f"\n=== {target} ===\n```rust\n{src}```\n"
body = {"model": model, "stream": False, "think": False,
        "options": {"temperature": 0.1, "num_predict": 12000, "num_ctx": NUM_CTX},
        "messages": [{"role": "user", "content": prompt}]}
t = time.time()
req = urllib.request.Request(HOST + "/api/chat", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
r = json.load(urllib.request.urlopen(req, timeout=3000))
text = r["message"]["content"]
i = text.find("```rust\n")
code = text[i + 8:] if i >= 0 else text
# Zamykające ogrodzenie to OSTATNIE: sam plik może zawierać ``` w komentarzach dokumentacyjnych.
j = code.rfind("\n```")
code = code[:j + 1] if j >= 0 else code
open(target + ".llm", "w").write(code)
secs, toks = time.time() - t, r.get("eval_count", 0)
impl = code[:code.index(marker)] if marker in code else code
same = marker in code and marker in src and code[code.index(marker):].strip() == src[src.index(marker):].strip()
bad = len(re.findall(r"\.unwrap\(\)|\.expect\(|panic!", impl))
print(f"{model}: {secs:.0f} s, {toks} tok ({toks/max(secs,1):.0f} tok/s), koniec={r.get('done_reason')}")
print(f"moduł testów niezmieniony: {same} | unwrap/expect/panic poza testami: {bad} | wynik: {target}.llm")
if not same:
    print("UWAGA: model zmienił moduł testów albo uciął odpowiedź; użyj tylko implementacji i dołącz oryginalne testy.")
