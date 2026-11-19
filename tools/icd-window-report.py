#!/usr/bin/env python3
"""Raport okien 5-sekundowych z logu klienta zdalnego Vulkana (gpu-icd).

Użycie: tools/icd-window-report.py <log> [--from N] [--to M] [--csv]

Skrypt łączy kolejne wystąpienia cyklicznych linii "statystyki" w okna 5 s
i dla każdego okna wypisuje: klatki, FPS, oczekiwanie na odpowiedzi serwera
(ms/klatkę), skan pamięci (ms/klatkę), obiegi po sieci/klatkę, chybienia
deskryptorów na serwer, p50/p95 odstępów oddania i wyświetlenia oraz trzy
najkosztowniejsze polecenia w oknie. Liczniki są narastające — liczone są
delty względem poprzedniego okna. Brakujące pola dają "-". Na końcu
drukowane jest podsumowanie (suma klatek, średnie ważone). Flaga --csv
zmienia format wyjścia na CSV.
"""
import argparse
import csv
import re
import sys

FIELD_SEP = "|"
NUM = r"-?\d+(?:\.\d+)?"


def num(s):
    try:
        return float(s)
    except (TypeError, ValueError):
        return None


def _search(pat, text, group=1, cast=num):
    m = re.search(pat, text)
    if not m:
        return None
    try:
        return cast(m.group(group))
    except (ValueError, IndexError, TypeError):
        return None


def parse_stat(line):
    """Parsuje linię 'statystyki: ...' do słownika pól liczbowych."""
    d = {}
    d["klatki"] = _search(r"klatek\s+(\d+):", line)
    d["serwer_ms"] = _search(r"oczekiwanie na odpowiedzi serwera\s+(" + NUM + r")\s*ms", line)
    d["skan_ms"] = _search(r"skan pamięci\s+(" + NUM + r")\s*ms", line)
    d["obiegi"] = _search(r"obiegi po sieci\s+(\d+)", line)
    m = re.search(r"deskryptory\s*\(lokalnie/cache/serwer\)\s*\d+/\d+/(\d+)", line)
    d["desk_serwer"] = int(m.group(1)) if m else None
    return d


def parse_latency(line):
    d = {}
    d["p50_odd"] = _search(r"odstępy oddania:\s*p50\s+(" + NUM + r")", line)
    d["p95_odd"] = _search(r"odstępy oddania:[^;]*?p95\s+(" + NUM + r")", line)
    d["p50_wys"] = _search(r"odstępy wyświetlenia:\s*p50\s+(" + NUM + r")", line)
    d["p95_wys"] = _search(r"odstępy wyświetlenia:[^;]*?p95\s+(" + NUM + r")", line)
    return d


CMD_RE = re.compile(r"([A-Za-z_][\w]*)\s+(\d+)x\s+(" + NUM + r")\s*ms")


def parse_cmds(line):
    out = {}
    for m in CMD_RE.finditer(line):
        name = m.group(1)
        out[name] = (int(m.group(2)), float(m.group(3)))
    return out


def fmt(v, spec=None):
    if v is None:
        return "-"
    if spec:
        try:
            return format(v, spec)
        except (ValueError, TypeError):
            return "-"
    return str(v)


def main(argv=None):
    ap = argparse.ArgumentParser(description="Raport okien 5 s z logu gpu-icd.")
    ap.add_argument("log")
    ap.add_argument("--from", dest="frm", type=int, default=None)
    ap.add_argument("--to", dest="to", type=int, default=None)
    ap.add_argument("--csv", action="store_true")
    args = ap.parse_args(argv)

    # Zbieranie danych: kolejne linie statystyk + opóźnień + poleceń w kolejności.
    stat_lines = []
    lat_lines = []
    cmd_lines = []
    try:
        with open(args.log, "r", encoding="utf-8", errors="replace") as f:
            for raw in f:
                line = raw.rstrip("\n")
                if "gpu-icd statystyki:" in line:
                    stat_lines.append(line)
                elif "opóźnienie klatki" in line or "odstępy" in line:
                    lat_lines.append(line)
                elif "obiegi wg poleceń:" in line:
                    cmd_lines.append(line)
    except OSError as e:
        print(f"Błąd odczytu pliku: {e}", file=sys.stderr)
        return 1

    if not stat_lines:
        print("Brak linii statystyk w logu.", file=sys.stderr)
        return 1

    # Dopasuj linie opóźnień i poleceń do okien: najprościej w kolejności.
    n = len(stat_lines)
    stats = [parse_stat(l) for l in stat_lines]
    lats = [parse_latency(lat_lines[i]) if i < len(lat_lines) else {} for i in range(n)]

    # Polecenia narastające: trzymaj ostatnią wartość na nazwę, licząc delty per okno.
    cmds_by_window = []
    last_cmd = {}
    # Polecenia występują po statystykach tego samego okna (log: stat, opóźnienie, polecenia).
    # Grupujemy w kolejności: dla każdego okna i bierzemy bloki poleceń aż do następnej statystyki.
    # Uproszczenie: każda linia poleceń przypisana do najbliższego kolejnego okna o numerze
    # wynikającym z pozycji — użyjemy przybliżenia: i-te linie poleceń do i-tego okna (jeśli są).
    for i in range(n):
        raw = cmd_lines[i] if i < len(cmd_lines) else ""
        parsed = parse_cmds(raw) if raw else {}
        deltas = {}
        for name, (cnt, ms) in parsed.items():
            pc, pms = last_cmd.get(name, (0, 0.0))
            d_cnt = cnt - pc
            d_ms = ms - pms
            if d_cnt > 0 or d_ms > 0:
                deltas[name] = d_ms
            last_cmd[name] = (cnt, ms)
        cmds_by_window.append(deltas)

    # Delty narastających liczników.
    def delta(cur, prev):
        if cur is None or prev is None:
            return None
        return cur - prev

    def fmt_delta(v, spec=None, scale=None):
        if v is None:
            return "-"
        if scale:
            v = v / scale
        return fmt(v, spec)

    rows = []
    prev = None
    total_frames = 0
    # Do średnich ważonych: (suma_klatek, suma_liczników)
    sums = {k: [0.0, 0.0] for k in ("serwer_ms", "skan_ms", "obiegi", "desk_serwer")}

    for i in range(n):
        s = stats[i]
        k = s.get("klatki")
        row = {"okno": i + 1}
        if prev is None:
            row["klatek"] = "-"
            row["fps"] = "-"
            row["serwer"] = "-"
            row["skan"] = "-"
            row["obiegi"] = "-"
            row["desk"] = "-"
        else:
            dk = delta(k, prev.get("klatki"))
            row["klatek"] = fmt_delta(dk, ".0f")
            row["fps"] = fmt_delta(dk, ".1f", 5.0)
            ds = delta(s.get("serwer_ms"), prev.get("serwer_ms"))
            dsk = delta(s.get("skan_ms"), prev.get("skan_ms"))
            do = delta(s.get("obiegi"), prev.get("obiegi"))
            dd = delta(s.get("desk_serwer"), prev.get("desk_serwer"))
            row["serwer"] = fmt_delta(ds, ".1f", dk) if ds is not None and dk else "-"
            row["skan"] = fmt_delta(dsk, ".1f", dk) if dsk is not None and dk else "-"
            row["obiegi"] = fmt_delta(do, ".1f", dk) if do is not None and dk else "-"
            row["desk"] = fmt_delta(dd, "d")
            # sumy ważone
            if dk:
                for key, val in (("serwer_ms", ds), ("skan_ms", dsk), ("obiegi", do), ("desk_serwer", dd)):
                    if val is not None:
                        sums[key][0] += val
                        sums[key][1] += dk
                total_frames += int(dk)

        l = lats[i] or {}
        row["p50_odd"] = fmt(l.get("p50_odd"), ".1f")
        row["p95_odd"] = fmt(l.get("p95_odd"), ".1f")
        row["p50_wys"] = fmt(l.get("p50_wys"), ".1f")
        row["p95_wys"] = fmt(l.get("p95_wys"), ".1f")

        # Trzy najkosztowniejsze polecenia w oknie.
        deltas = cmds_by_window[i]
        top = sorted(deltas.items(), key=lambda kv: kv[1], reverse=True)[:3]
        row["top"] = ", ".join(f"{name} {ms:.0f}ms" for name, ms in top) if top else "-"

        rows.append(row)
        prev = s

    # Podsumowanie
    summary = {"total_frames": total_frames}
    for key in sums:
        s_sum, w_sum = sums[key]
        summary[key] = (s_sum / w_sum) if w_sum else None

    # Filtrowanie --from/--to (po numerze okna)
    def in_range(i):
        if args.frm is not None and i + 1 < args.frm:
            return False
        if args.to is not None and i + 1 > args.to:
            return False
        return True

    filtered = [r for i, r in enumerate(rows) if in_range(i)]

    headers = ["okno", "klatek", "fps", "serwer_ms/klatke", "skan_ms/klatke",
               "obiegi/klatke", "desk_serwer", "p50_odd", "p95_odd",
               "p50_wys", "p95_wys", "top3"]

    def row_values(r):
        return [r["okno"], r["klatek"], r["fps"], r["serwer"], r["skan"],
                r["obiegi"], r["desk"], r["p50_odd"], r["p95_odd"],
                r["p50_wys"], r["p95_wys"], r["top"]]

    if args.csv:
        w = csv.writer(sys.stdout)
        w.writerow(headers)
        for r in filtered:
            w.writerow(row_values(r))
        # wiersz podsumowania
        w.writerow(["SUMA", summary["total_frames"],
                    fmt(summary["serwer_ms"], ".1f"),
                    fmt(summary["skan_ms"], ".1f"),
                    fmt(summary["obiegi"], ".1f"),
                    fmt(summary["desk_serwer"], ".2f"),
                    "", "", "", "", ""])
    else:
        # Tabela tekstowa
        print(FIELD_SEP.join(headers))
        print(FIELD_SEP.join("-" * len(h) for h in headers))
        for r in filtered:
            print(FIELD_SEP.join(str(v) for v in row_values(r)))
        print(FIELD_SEP.join([
            "SUMA",
            str(summary["total_frames"]),
            "",  # fps nieistotne w podsumowaniu
            fmt(summary["serwer_ms"], ".1f"),
            fmt(summary["skan_ms"], ".1f"),
            fmt(summary["obiegi"], ".1f"),
            fmt(summary["desk_serwer"], ".2f"),
            "", "", "", "", ""]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
