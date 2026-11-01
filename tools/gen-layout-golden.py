#!/usr/bin/env python3
"""Generuje wzorcowy plik układu pamięci struktur Vulkana z prawdziwego kompilatora C.

Wejście: third_party/vk.xml + third_party/vulkan-headers (Khronos, Apache-2.0 OR MIT).
Wyjście: crates/gpu-vkgen/tests/data/layout-golden.tsv
  S <TAB> nazwa <TAB> sizeof <TAB> alignof
  M <TAB> nazwa <TAB> pole <TAB> offsetof        (bez pól bitowych)
Narzędzie deweloperskie: produkt go nie używa. Wymaga gcc.
"""
import re, subprocess, sys, tempfile, os
import xml.etree.ElementTree as ET

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HDR = f"{ROOT}/third_party/vulkan-headers"
core = open(f"{HDR}/vulkan/vulkan_core.h").read()
in_header = set(re.findall(r'^typedef (?:struct|union) (Vk\w+) \{', core, re.M))

reg = ET.parse(f"{ROOT}/third_party/vk.xml").getroot()
structs = {}
for t in reg.findall("./types/type"):
    if t.get("category") in ("struct", "union") and not t.get("alias"):
        name = t.get("name")
        if name in in_header:
            members = []
            for m in t.findall("member"):
                if m.get("api") and "vulkan" not in m.get("api").split(","):
                    continue
                n = m.find("name").text
                text = "".join(m.itertext())
                members.append((n, ":" in text.split(n, 1)[-1][:6]))  # pole bitowe: "name:24"
            structs[name] = members

c = ["#include <stdio.h>", "#include <stddef.h>", '#include "vulkan/vulkan_core.h"', "int main(void) {"]
for name, members in sorted(structs.items()):
    c.append(f'  printf("S\\t{name}\\t%zu\\t%zu\\n", sizeof({name}), _Alignof({name}));')
    for n, bit in members:
        if not bit:
            c.append(f'  printf("M\\t{name}\\t{n}\\t%zu\\n", offsetof({name}, {n}));')
c.append("  return 0; }")
with tempfile.TemporaryDirectory() as d:
    src, exe = f"{d}/g.c", f"{d}/g"
    open(src, "w").write("\n".join(c))
    r = subprocess.run(["gcc", "-std=c11", "-O0", f"-I{HDR}", src, "-o", exe], capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[:3000]); sys.exit(1)
    out = subprocess.run([exe], capture_output=True, text=True, check=True).stdout
os.makedirs(f"{ROOT}/crates/gpu-vkgen/tests/data", exist_ok=True)
open(f"{ROOT}/crates/gpu-vkgen/tests/data/layout-golden.tsv", "w").write(out)
print(f"struktur: {len(structs)}, linii: {out.count(chr(10))}, bajtow: {len(out)}")
