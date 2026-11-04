#!/usr/bin/env python3
"""Compares a clipsave file byte-for-byte with the back-ticked string of a documented
"| Copy offset - <tag>" line of tests/tools/expected_clipboard.md.
usage: python3 -I tests/tools/compare-clip.py <clipsave.txt> "<tag>"  (prints MATCH / MISMATCH, exit 1 on mismatch)"""
import sys, os

clip = open(sys.argv[1], "rb").read()
tag = sys.argv[2]
doc = os.path.join(os.path.dirname(os.path.abspath(__file__)), "expected_clipboard.md")
lines = [l for l in open(doc, encoding="utf-8").read().split("\n") if l.startswith("| Copy offset - ") and tag in l]
if len(lines) != 1:
    sys.exit("expected exactly one documented line for %r, got %d" % (tag, len(lines)))
want = lines[0].split("`")[1].encode("utf-8")
print("MATCH" if clip == want else "MISMATCH", "clip=%r want=%r" % (clip.decode("utf-8", "replace"), want.decode("utf-8")))
sys.exit(0 if clip == want else 1)
