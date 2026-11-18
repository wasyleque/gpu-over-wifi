#!/usr/bin/env python3
"""Zlecenie implementacji modelowi DeepSeek (API zgodne z OpenAI): wypełnia todo!() w pliku z czerwonymi testami.

Użycie: delegate-deepseek.py <zlecenie.txt> <plik.rs> [plik-tylko-do-odczytu ...] [--model deepseek-flash|deepseek-v4-pro]
Klucz API: plik ~/.config/deepseek/key (uprawnienia 600) albo zmienna DEEPSEEK_API_KEY; nigdy nie jest wypisywany ani zapisywany w repo.
Wynik trafia do <plik.rs>.llm (NIE nadpisuje pliku): skrypt sprawdza, że moduł testów jest identyczny, liczy zakazane konstrukcje
(unwrap/expect/panic poza testami) i drukuje statystyki tokenów oraz szacunkowy koszt. Przegląd i kopiowanie robi człowiek.

UWAGA: treść podanych plików jest wysyłana do zewnętrznej usługi (serwery DeepSeek). Podawaj tylko pliki, które mogą tam trafić.
"""
import json, os, re, sys, time, urllib.error, urllib.request

HOST = "https://api.deepseek.com/chat/completions"
DEFAULT_MODEL = "deepseek-flash"
# USD za 1 mln tokenów (poza szczytem; szczyt jest 2x droższy): (wejście bez cache, wejście z cache, wyjście)
PRICE = {"deepseek-flash": (0.15, 0.003, 0.6), "deepseek-v4-pro": (0.66, 0.022, 1.98)}

args = sys.argv[1:]
model = DEFAULT_MODEL
if "--model" in args:
    i = args.index("--model"); model = args[i + 1]; del args[i:i + 2]
if len(args) < 2:
    sys.exit(__doc__)
task, target, readonly = args[0], args[1], args[2:]

key = os.environ.get("DEEPSEEK_API_KEY") or ""
if not key:
    try:
        key = open(os.path.expanduser("~/.config/deepseek/key")).read().strip()
    except OSError:
        sys.exit("Brak klucza: zapisz go w ~/.config/deepseek/key (chmod 600) albo ustaw DEEPSEEK_API_KEY.")

src = open(target).read()
marker = "#[cfg(test)]\nmod tests"
prompt = open(task).read() + "\nReturn ONLY the complete file in one ```rust block, nothing else.\n"
for r in readonly:
    prompt += f"\n=== {r} (read-only context) ===\n```rust\n{open(r).read()}```\n"
prompt += f"\n=== {target} ===\n```rust\n{src}```\n"
body = {"model": model, "stream": False, "temperature": 0.1, "max_tokens": 12000,
        "messages": [{"role": "user", "content": prompt}]}
t = time.time()
req = urllib.request.Request(HOST, data=json.dumps(body).encode(),
                             headers={"Content-Type": "application/json", "Authorization": "Bearer " + key})
try:
    r = json.load(urllib.request.urlopen(req, timeout=900))
except urllib.error.HTTPError as e:
    sys.exit(f"Błąd HTTP {e.code}: {e.read().decode(errors='replace')[:300]}")
text = r["choices"][0]["message"]["content"]
i = text.find("```rust\n")
code = text[i + 8:] if i >= 0 else text
# Zamykające ogrodzenie to OSTATNIE: sam plik może zawierać ``` w komentarzach dokumentacyjnych.
j = code.rfind("\n```")
code = code[:j + 1] if j >= 0 else code
open(target + ".llm", "w").write(code)
secs = time.time() - t
u = r.get("usage", {})
hit, miss, out = u.get("prompt_cache_hit_tokens", 0), u.get("prompt_cache_miss_tokens", u.get("prompt_tokens", 0)), u.get("completion_tokens", 0)
pm, ph, po = PRICE.get(model, (0, 0, 0))
cost = (miss * pm + hit * ph + out * po) / 1e6
impl = code[:code.index(marker)] if marker in code else code
same = marker in code and marker in src and code[code.index(marker):].strip() == src[src.index(marker):].strip()
bad = len(re.findall(r"\.unwrap\(\)|\.expect\(|panic!", impl))
print(f"{model}: {secs:.0f} s, wejście {miss}+{hit} (cache) tok, wyjście {out} tok, koszt ok. ${cost:.4f} (stawki poza szczytem), koniec={r['choices'][0].get('finish_reason')}")
print(f"moduł testów niezmieniony: {same} | unwrap/expect/panic poza testami: {bad} | wynik: {target}.llm")
if not same:
    print("UWAGA: model zmienił moduł testów albo uciął odpowiedź; użyj tylko implementacji i dołącz oryginalne testy.")
