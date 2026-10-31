#!/usr/bin/env python3
"""Checks layout-table "Gap before" cells in a dump directory (made by tests/tools/dump-all.sh).
usage: python3 -I tests/tools/gap-check.py <dump-dir>
Reports: files containing 'autistic'; layout rows whose non-zero Gap cell equals their own Start (decimal==hex);
per file with a 'Layout group' page >= 2: the Gap of row 0 of each such page and the 'N unaccounted gap(s)' header."""
import os, re, sys

d = sys.argv[1]
autistic, selfgap, pages, files = [], [], [], 0
for name in sorted(os.listdir(d)):
    files += 1
    lines = open(os.path.join(d, name), encoding="utf-8", errors="replace").read().split("\n")
    if any("autistic" in l.lower() for l in lines):
        autistic.append(name)
    hdr = next((re.search(r"(\d+) unaccounted gap", l).group(1) for l in lines if "unaccounted gap" in l), None)
    first_page = True
    in_group = False
    row0_pending = False
    for l in lines:
        if "== Layout group:" in l:
            in_group = True
            row0_pending = True
            m = re.search(r"items (\d+)\.\.", l)
            page_start = int(m.group(1)) if m else 0
            continue
        cells = [c.strip() for c in l.split("|")]
        if len(cells) >= 9 and re.fullmatch(r"0x[0-9a-f]+", cells[1]) and re.fullmatch(r"\d+|-", cells[6]):
            gap = cells[6]
            start = int(cells[1], 16)
            if gap != "-" and int(gap) != 0 and int(gap) == start:
                selfgap.append((name, l.strip()[:120]))
            if row0_pending and in_group:
                if page_start > 0:
                    pages.append((name, page_start, gap, hdr))
                row0_pending = False
print(f"dump files scanned: {files}")
print(f"files containing 'autistic': {len(autistic)} {autistic}")
print(f"layout rows with non-zero Gap == own Start: {len(selfgap)}")
for n, l in selfgap:
    print("  ", n, l)
print(f"group pages after the first: {len(pages)} in {len(set(p[0] for p in pages))} files")
for n, s, g, h in pages:
    print(f"  {n} page@item{s} row0 Gap={g} header unaccounted={h}")
