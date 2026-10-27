#!/usr/bin/env python3
"""Classify the differences between two dump directories (made by tests/tools/dump-all.sh).
usage: python3 -I tests/tools/dump-classify.py <old-dir> <new-dir>
Classes: STATUS (a status cell changed to 'Not checked' / '✖ Invalid'), GROUP-LABEL, COMPRESSION-LINE, NOTE, ORDER (same line, other place),
OTHER (anything else; printed in full)."""
import os, re, sys, collections

old, new = sys.argv[1], sys.argv[2]
cls = collections.Counter()
files = collections.defaultdict(set)
other = []
changed = 0

def status_swap(r):
    return re.sub(r"(✔ Present|\? Unknown)$", "Not checked", r)

for name in sorted(os.listdir(old)):
    a = open(os.path.join(old, name), encoding="utf-8", errors="replace").read().split("\n")
    b = open(os.path.join(new, name), encoding="utf-8", errors="replace").read().split("\n")
    if a == b:
        continue
    changed += 1
    ca, cb = collections.Counter(a), collections.Counter(b)
    R = list((ca - cb).elements())
    A = list((cb - ca).elements())
    moved = sum(((ca & cb)).values())
    if collections.Counter(a) == collections.Counter(b):
        cls["ORDER"] += 1
        files["ORDER"].add(name)
        continue
    # lines identical but moved are not in R/A, so ORDER is only counted when everything else is equal
    A_left = list(A)
    for r in R:
        done = None
        s = status_swap(r)
        if s in A_left:
            done = "STATUS"; A_left.remove(s)
        else:
            m = re.match(r"^(\s*\+?\s*)(Data blocks #\d+–#\d+) (\[0x[0-9a-f]+,0x[0-9a-f]+\)) · (\d+ blocks) · not checked(.*)$", r)
            m2 = re.match(r"^(\s*\+?\s*)(Data blocks #\d+–#\d+) (\[0x[0-9a-f]+,0x[0-9a-f]+\)) · (\d+ blocks) · not checked (.*)$", r)
            if m:
                cand = f"{m.group(1)}{m.group(2)} · not checked · {m.group(3)} · {m.group(4)}{m.group(5)}"
                cand2 = status_swap(cand)
                for c in (cand, cand2, cand.replace(" [not checked]", " [not checked]")):
                    if c in A_left:
                        done = "GROUP-LABEL"; A_left.remove(c); break
            if not done and "Actual block compression" in r:
                m = re.match(r"^(.*\| )(\d+) (\w+) \((\d+) of (\d+) data blocks checked\)( \|.*)$", r)
                if m:
                    pat = re.compile(re.escape(m.group(1)) + r"\d+ \w+ \(type byte read for \d+ of \d+ data blocks\)" + re.escape(m.group(6)) + "$")
                    for c in A_left:
                        if pat.match(c):
                            done = "COMPRESSION-LINE"; A_left.remove(c); break
            if not done and r.strip().startswith("note:"):
                for c in A_left:
                    if c.strip().startswith("note:"):
                        done = "NOTE"; A_left.remove(c); break
        if done:
            cls[done] += 1
            files[done].add(name)
        else:
            other.append((name, r))
    for c in A_left:
        # added lines whose removed partner was not matched above
        other.append((name, "+ " + c))

print(f"files differing: {changed}")
for k, v in cls.items():
    print(f"{k}: {v} lines in {len(files[k])} files")
print(f"OTHER: {len(other)} lines")
seen = collections.Counter()
for n, l in other:
    key = re.sub(r"0x[0-9a-f]+", "0xH", re.sub(r"\d+", "N", l))
    seen[key] += 1
for k, v in seen.most_common(40):
    print(f"  {v}x {k[:230]}")
