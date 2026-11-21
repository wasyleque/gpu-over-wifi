#!/usr/bin/env python3
"""Tłumaczenie dokumentów na en / pl / es przez DeepSeek (przed każdym wypchnięciem: tools/release.sh).

Lista dokumentów i ich język źródłowy: tools/i18n-docs.json. Dla `X.md` powstają `X.<język>.md` pozostałych języków (np. README.pl.md, README.es.md).
Tłumaczy tylko dokumenty ZMIENIONE od ostatniego tłumaczenia (skrót SHA-256 źródła w docs/.i18n-hashes.json). Wynik jest sprawdzany strukturalnie
(liczba nagłówków, ogrodzeń kodu, wierszy tabel, te same adresy URL i fragmenty `kodu`); przy niezgodności jedna powtórka, potem błąd (kod wyjścia 1).
Klucz API: ~/.config/deepseek/key lub DEEPSEEK_API_KEY (nigdy nie jest wypisywany). Użycie: translate-docs.py [--check] [--force]
  --check  tylko sprawdza, czy tłumaczenia są aktualne (kod 1, gdy nie); --force  tłumaczy wszystko od nowa.
UWAGA: treść dokumentów jest wysyłana do usługi DeepSeek; w tych plikach nie ma być sekretów ani danych prywatnych.
"""
import hashlib, json, os, re, sys, time, urllib.request, concurrent.futures as cf, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
CFG = json.loads((ROOT / "tools/i18n-docs.json").read_text())
HASHES = ROOT / "docs/.i18n-hashes.json"
NAMES = {"en": "English", "pl": "Polski", "es": "Español"}
LANGNAME = {"en": "English", "pl": "Polish", "es": "Spanish (neutral, international)"}
MODEL = "deepseek-v4-pro"
SW_START, SW_END = "<!-- i18n-switch -->", "<!-- /i18n-switch -->"


def variant(path: str, lang: str, src_lang: str) -> str:
    if lang == src_lang:
        return path
    return path[:-3] + "." + lang + ".md"


def switcher(path: str, src_lang: str, cur: str) -> str:
    base = pathlib.Path(path).name
    parts = []
    for l in CFG["langs"]:
        name = pathlib.Path(variant(path, l, src_lang)).name
        parts.append(f"**{NAMES[l]}**" if l == cur else f"[{NAMES[l]}]({name})")
    return f"{SW_START}\n{' · '.join(parts)}\n{SW_END}\n"


def strip_switch(text: str) -> str:
    return re.sub(re.escape(SW_START) + r".*?" + re.escape(SW_END) + r"\n*", "", text, flags=re.S)


def with_switch(text: str, path: str, src_lang: str, cur: str) -> str:
    return switcher(path, src_lang, cur) + "\n" + strip_switch(text).lstrip("\n")


def key() -> str:
    k = os.environ.get("DEEPSEEK_API_KEY")
    if k:
        return k.strip()
    return (pathlib.Path.home() / ".config/deepseek/key").read_text().strip()


def sig(text: str):
    body = strip_switch(text)
    urls = sorted(re.findall(r"\]\(([^)\s]+)\)", body)) + sorted(re.findall(r"https?://[^\s)>\]]+", body))
    codes = sorted(re.findall(r"`([^`\n]+)`", body))
    return {
        "headings": len(re.findall(r"^#{1,6} ", body, flags=re.M)),
        "fences": body.count("```"),
        "table_rows": len(re.findall(r"^\|", body, flags=re.M)),
        "urls": urls,
        "codes": codes,
    }


def check(src: str, out: str):
    a, b = sig(src), sig(out)
    bad = []
    for k in ("headings", "fences", "table_rows"):
        if a[k] != b[k]:
            bad.append(f"{k}: {a[k]} != {b[k]}")
    if a["urls"] != b["urls"]:
        bad.append("adresy URL/linki różnią się")
    # fragmenty `kodu` mają zostać bez zmian (dopuszczamy ok. 12%: placeholdery <ADRES> i nazwy przykładowych plików bywają tłumaczone)
    miss = [c for c in a["codes"] if c not in b["codes"]]
    if len(miss) > max(3, len(a["codes"]) // 8):
        bad.append(f"zmienione fragmenty kodu: {miss[:5]}")
    return bad


PROMPT = """You are a professional technical translator. Translate the following Markdown document from {src} to {dst}.
Rules:
- Output ONLY the translated Markdown document, no commentary, no code fences around it.
- Keep the Markdown structure EXACTLY: same headings (translated), same number of table rows and columns, same lists, same blank lines.
- The text contains opaque markers like ⟦12⟧ (protected code, links, URLs). Keep EVERY marker exactly once, unchanged, in the same logical place; never translate or reorder their content.
- Do NOT translate or alter: code blocks, inline `code`, file names and paths, command lines, environment variables, URLs and link targets, image paths, product and game names (Cyberpunk 2077, Rise of the Tomb Raider, vkd3d-proton, Proton, Vulkan, NVENC, VA-API, etc.), numbers, units, dates, hardware model names.
- Translate link TEXT and image alt text, keep the link targets unchanged.
- Keep an engineering tone, precise and concise; keep decimal separators as in the source for tables of measurements (use a decimal point in English and Spanish tables? NO: keep numbers exactly as written in the source).
- Text quoted from program output or log lines (e.g. inside `code`) must stay in its original language; only placeholders like <ADDRESS> may be translated.
- Preserve HTML comments and any bold/italic emphasis.

DOCUMENT:
"""


PROT = re.compile(r"```.*?```|`[^`\n]+`|\]\([^)\s]+\)|https?://[^\s)>\]]+|<a id=\"[^\"]+\"></a>", re.S)


def protect(text: str):
    """Fragmenty, których nie wolno zmieniać (kod, linki, adresy), zastępujemy znacznikami ⟦n⟧ i przywracamy po tłumaczeniu."""
    items = []

    def rep(m):
        items.append(m.group(0))
        return f"⟦{len(items) - 1}⟧"
    return PROT.sub(rep, text), items


def restore(text: str, items):
    for i, it in enumerate(items):
        if text.count(f"⟦{i}⟧") != 1:
            raise RuntimeError(f"znacznik ⟦{i}⟧ występuje {text.count(f'⟦{i}⟧')} razy")
        text = text.replace(f"⟦{i}⟧", it)
    if "⟦" in text:
        raise RuntimeError("nadmiarowe znaczniki w wyniku")
    return text


def call(text: str, src: str, dst: str) -> str:
    body = {"model": MODEL, "stream": False, "temperature": 0.1, "max_tokens": 30000, "thinking": {"type": "disabled"},
            "messages": [{"role": "user", "content": PROMPT.format(src=LANGNAME[src], dst=LANGNAME[dst]) + text}]}
    for attempt in range(3):
        try:
            req = urllib.request.Request("https://api.deepseek.com/chat/completions", data=json.dumps(body).encode(),
                                         headers={"Authorization": "Bearer " + key(), "Content-Type": "application/json"})
            r = json.load(urllib.request.urlopen(req, timeout=900))
            out = r["choices"][0]["message"]["content"]
            if r["choices"][0].get("finish_reason") == "length":
                raise RuntimeError("odpowiedź ucięta (finish_reason=length)")
            return out.strip("\n") + "\n"
        except Exception as e:  # noqa: BLE001
            if attempt == 2:
                raise
            print(f"  ponawiam po błędzie: {type(e).__name__}", file=sys.stderr)
            time.sleep(5 * (attempt + 1))
    raise RuntimeError("unreachable")


def translate_one(path: str, src_lang: str, lang: str, text: str) -> str:
    last = []
    masked, items = protect(text)
    for _ in range(3):
        try:
            out = restore(call(masked, src_lang, lang), items)
        except RuntimeError as e:
            last = [str(e)]
            print(f"  {path} -> {lang}: {e}", file=sys.stderr)
            continue
        last = check(text, out)
        if not last:
            return out
        print(f"  niezgodność struktury ({path} -> {lang}): {last}", file=sys.stderr)
    raise RuntimeError(f"{path} -> {lang}: tłumaczenie nie przeszło kontroli: {last}")


def main():
    only_check = "--check" in sys.argv
    force = "--force" in sys.argv
    hashes = json.loads(HASHES.read_text()) if HASHES.exists() else {}
    jobs, stale = [], []
    for d in CFG["docs"]:
        path, src_lang = d["path"], d["lang"]
        f = ROOT / path
        raw = f.read_text()
        h = hashlib.sha256(strip_switch(raw).encode()).hexdigest()
        for lang in CFG["langs"]:
            if lang == src_lang:
                continue
            target = ROOT / variant(path, lang, src_lang)
            if force or hashes.get(path) != h or not target.exists():
                stale.append(f"{path} -> {lang}")
                jobs.append((path, src_lang, lang, strip_switch(raw), h))
    if only_check:
        # przełączniki w źródłach też muszą być aktualne
        for d in CFG["docs"]:
            raw = (ROOT / d["path"]).read_text()
            if with_switch(raw, d["path"], d["lang"], d["lang"]) != raw:
                stale.append(f"{d['path']} (przełącznik języków)")
        if stale:
            print("Nieaktualne tłumaczenia:\n  " + "\n  ".join(stale))
            sys.exit(1)
        print("Tłumaczenia aktualne.")
        return
    if not jobs:
        print("Tłumaczenia aktualne, nic do zrobienia.")
    results = {}
    with cf.ThreadPoolExecutor(max_workers=3) as ex:
        futs = {ex.submit(translate_one, p, s, l, t): (p, s, l, h) for p, s, l, t, h in jobs}
        failed = []
        for fu in cf.as_completed(futs):
            p, s, l, h = futs[fu]
            try:
                results[(p, l)] = (fu.result(), s, h)
                print(f"  przetłumaczono {p} -> {l}")
            except Exception as e:  # noqa: BLE001
                failed.append(f"{p} -> {l}: {e}")
    for (p, l), (out, s, h) in results.items():
        (ROOT / variant(p, l, s)).write_text(with_switch(out, p, s, l))
    if jobs and failed:
        print("BŁĄD tłumaczeń:\n  " + "\n  ".join(failed), file=sys.stderr)
        sys.exit(1)
    # źródła: przełącznik języków i zapis skrótów
    for d in CFG["docs"]:
        f = ROOT / d["path"]
        raw = f.read_text()
        f.write_text(with_switch(raw, d["path"], d["lang"], d["lang"]))
        hashes[d["path"]] = hashlib.sha256(strip_switch(f.read_text()).encode()).hexdigest()
    HASHES.write_text(json.dumps(hashes, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
