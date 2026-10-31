#!/usr/bin/env python3
"""Copy SRC to DST and XOR one byte at OFFSET with 0xFF (corrupt-fixture helper).

Usage: python3 -I tests/tools/flip-byte.py <src> <dst> <offset>
SRC is only read. Refuses to run if DST exists or OFFSET is out of range.
Prints old/new byte and the sha256 of DST. OFFSET may be decimal or 0x-hex.
"""
import hashlib
import os
import shutil
import sys


def main(argv):
    if len(argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    src, dst, off = argv[1], argv[2], int(argv[3], 0)
    if os.path.exists(dst):
        print(f"refusing: {dst} already exists", file=sys.stderr)
        return 1
    size = os.path.getsize(src)
    if not 0 <= off < size:
        print(f"offset {off} outside file of {size} bytes", file=sys.stderr)
        return 1
    shutil.copyfile(src, dst)
    with open(dst, "r+b") as f:
        f.seek(off)
        old = f.read(1)[0]
        new = old ^ 0xFF
        f.seek(off)
        f.write(bytes([new]))
    with open(dst, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    print(f"offset {off} (0x{off:x}): 0x{old:02x} -> 0x{new:02x}")
    print(f"size {size} sha256 {digest}  {dst}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
