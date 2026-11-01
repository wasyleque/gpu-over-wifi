#!/usr/bin/env python3
import sys
import os
from collections import Counter

def main():
    if len(sys.argv) != 2:
        print("Użycie: trace-summary.py <plik_sladu>")
        sys.exit(1)
    
    path = sys.argv[1]
    if not os.path.isfile(path):
        print(f"Błąd: plik '{path}' nie istnieje.")
        sys.exit(1)
    
    try:
        with open(path, 'r', encoding='utf-8', errors='replace') as f:
            lines = f.readlines()
    except Exception as e:
        print(f"Błąd odczytu pliku: {e}")
        sys.exit(1)
    
    micros = {}
    commands = []  # lista (numer_linii, nazwa, wynik_hex)
    for i, line in enumerate(lines, start=1):
        stripped = line.strip()
        if not stripped:
            continue
        # pomijamy opisy (2 spacje na początku)
        if line.startswith('  '):
            continue
        # parsujemy: vkNazwa -> 0xWYNIK albo vkNazwa
        if ' -> ' in stripped:
            parts = stripped.split(' -> ', 1)
            name = parts[0].strip()
            result_hex = parts[1].strip()
            if ' [' in result_hex:
                result_hex, t = result_hex.split(' [', 1)
                result_hex = result_hex.strip()
                micros[name] = micros.get(name, 0) + int(t.rstrip('us]') or 0)
        else:
            name = stripped
            result_hex = None
        
        # sprawdzamy czy to poleceń vk
        if name.startswith('vk'):
            commands.append((i, name, result_hex))
    
    total = len(commands)
    print(f"(1) Liczba wszystkich poleceń: {total}")
    
    # (2) 10 najczęstszych nazw
    name_counter = Counter(name for _, name, _ in commands)
    top10 = name_counter.most_common(10)
    print("(2) 10 najczęstszych nazw:")
    for name, count in top10:
        print(f"    {name}: {count}")
    
    # (3) wyniki ujemne (hex zaczynający się od 0xf lub 0x8), z wyjątkiem vkGetBufferDeviceAddress
    negative_lines = []
    for lineno, name, result_hex in commands:
        if result_hex is None:
            continue
        if name == 'vkGetBufferDeviceAddress':
            continue
        # sprawdzamy czy hex zaczyna się od 0xf lub 0x8
        rh = result_hex.lower()
        if rh.startswith('0xf') or rh.startswith('0x8'):
            negative_lines.append((lineno, name, result_hex))
    
    print("(3) Linie z wynikiem ujemnym (max 20):")
    if not negative_lines:
        print("    brak")
    else:
        for lineno, name, result_hex in negative_lines[:20]:
            print(f"    linia {lineno}: {name} -> {result_hex}")
    
    if micros:
        print('(3b) Łączny czas (ms) wg polecenia, top 8:')
        for n, us in sorted(micros.items(), key=lambda x: -x[1])[:8]:
            print(f'    {n}: {us/1000:.0f} ms')
    # (4) ostatnie 5 poleceń
    print("(4) Ostatnie 5 poleceń:")
    last5 = commands[-5:]
    if not last5:
        print("    brak")
    else:
        for lineno, name, result_hex in last5:
            if result_hex is not None:
                print(f"    linia {lineno}: {name} -> {result_hex}")
            else:
                print(f"    linia {lineno}: {name}")

if __name__ == '__main__':
    main()
