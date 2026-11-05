#!/usr/bin/env python3
# Podsumowanie logu klienta gpu-icd (FPS, obiegi wg poleceń, blokada, czas serwera). Wygenerowany przez model na .11, sprawdzony na logu Rise of the Tomb Raider.
import re
import sys


def read_lines(path):
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        return f.readlines()


def last_line(lines, needle):
    for line in reversed(lines):
        if needle in line:
            return line
    return None


def md_table(headers, rows):
    out = []
    out.append("| " + " | ".join(headers) + " |")
    out.append("|" + "|".join([" --- "] * len(headers)) + "|")
    for r in rows:
        out.append("| " + " | ".join(str(x) for x in r) + " |")
    return "\n".join(out)


def main():
    if len(sys.argv) < 2:
        print("usage: icd-log-summary.py <client.log> [server.log]")
        return 1
    client_path = sys.argv[1]
    server_path = sys.argv[2] if len(sys.argv) > 2 else None

    client_lines = read_lines(client_path)

    # 1. FPS series
    print("## FPS")
    series = []
    current = []
    prev_n = None
    for line in client_lines:
        if "statystyki" not in line:
            continue
        m = re.search(r"klatek (\d+):", line)
        if not m:
            continue
        n = int(m.group(1))
        if prev_n is None:
            current.append(n)
        elif n < prev_n:
            series.append(current)
            current = [n]
        else:
            current.append(n)
        prev_n = n
    if current:
        series.append(current)

    if not series:
        print("(brak)")
    else:
        rows = []
        for s in series:
            frames = s[-1]
            fps = []
            for i in range(1, len(s)):
                d = s[i] - s[i - 1]
                if d > 0:
                    fps.append(d / 5.0)
            if fps:
                mean = sum(fps) / len(fps)
                mn = min(fps)
                mx = max(fps)
                samples = [int(round(x)) for x in fps]
            else:
                mean = mn = mx = 0
                samples = []
            rows.append((frames, round(mean, 2), round(mn, 2), round(mx, 2), samples))
        print(md_table(["klatek", "śr FPS", "min", "max", "próbki"], rows))

    # 2. obiegi wg poleceń
    print("\n## obiegi wg poleceń")
    line = last_line(client_lines, "obiegi wg poleceń:")
    if line is None:
        print("(brak)")
    else:
        seg = line.split(":", 1)[1]
        parts = [p.strip() for p in seg.split("; ") if p.strip()]
        rows = []
        for p in parts:
            m = re.match(r"(\S+) (\d+)x (\d+)ms \((\d+)µs\)", p)
            if m:
                rows.append((m.group(1), int(m.group(2)), int(m.group(3)), int(m.group(4))))
        rows.sort(key=lambda r: -r[2])
        rows = rows[:8]
        print(md_table(["polecenie", "wywołania", "ms", "µs"], rows))

    # 3. blokada instancji
    print("\n## blokada instancji")
    line = last_line(client_lines, "blokada instancji:")
    if line is None:
        print("(brak)")
    else:
        m = re.search(r"oczekiwanie (\d+) ms, trzymanie (\d+) ms w (\d+) wejściach, najdłuższe trzymanie ([\d.]+) ms", line)
        if m:
            print(md_table(["oczekiwanie ms", "trzymanie ms", "wejścia", "najdłuższe ms"],
                           [(m.group(1), m.group(2), m.group(3), m.group(4))]))
        else:
            print("(brak)")

    # 4. gpu-icd statystyki
    print("\n## gpu-icd statystyki")
    line = last_line(client_lines, "gpu-icd statystyki")
    if line is None:
        print("(brak)")
    else:
        m_wait = re.search(r"oczekiwanie na odpowiedzi serwera (\d+) ms", line)
        m_klat = re.search(r"klatek (\d+)", line)
        if m_wait and m_klat:
            wait = int(m_wait.group(1))
            klat = int(m_klat.group(1))
            per = wait / klat if klat else 0
            print(md_table(["oczekiwanie ms", "klatek", "ms/klatkę"],
                           [(wait, klat, round(per, 2))]))
        else:
            print("(brak)")

    # 5. WATCHDOG
    print("\n## WATCHDOG")
    cnt = sum(1 for l in client_lines if "WATCHDOG" in l)
    print(cnt)

    # server
    if server_path:
        server_lines = read_lines(server_path)

        print("\n## czas (od startu)")
        line = last_line(server_lines, "czas (od startu)")
        if line is None:
            print("(brak)")
        else:
            seg = line.split(":", 1)[1]
            rows = []
            for m in re.finditer(r"(\w+) (\d+)x (\d+)ms", seg):
                rows.append((m.group(1), int(m.group(2)), int(m.group(3))))
            rows.sort(key=lambda r: -r[2])
            print(md_table(["operacja", "wywołania", "ms"], rows))

        print("\n## kopia GPU odczytu pamięci")
        line = last_line(server_lines, "kopia GPU odczytu pamięci")
        if line is None:
            print("(brak)")
        else:
            print(line.strip())

    return 0


if __name__ == "__main__":
    sys.exit(main())
