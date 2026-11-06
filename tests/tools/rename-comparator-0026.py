#!/usr/bin/env python3
"""Copy a MANIFEST and rename its comparator to an unsupported name of the SAME length,
re-computing the CRC32C of every physical log record that contained the name.

Usage: python3 -I tests/tools/rename-comparator-0026.py <src-manifest> <dst-manifest>
Replaces b"leveldb.BytewiseComparator" with b"rocksvision.TestCmp0026" padded to the same length
("rocksvision.TestCmp0026" + "_" * 3). Legacy 7-byte record headers only (MANIFEST files).
Refuses to overwrite DST. Prints the number of records patched and the sha256 of DST.
"""
import hashlib
import os
import struct
import sys

OLD = b"leveldb.BytewiseComparator"
NEW = b"rocksvision.TestCmp0026___"
assert len(OLD) == len(NEW), (len(OLD), len(NEW))
BLOCK = 32768


def crc32c_table():
    t = []
    for i in range(256):
        c = i
        for _ in range(8):
            c = (c >> 1) ^ 0x82F63B78 if c & 1 else c >> 1
        t.append(c)
    return t


TABLE = crc32c_table()


def crc32c(data):
    c = 0xFFFFFFFF
    for b in data:
        c = TABLE[(c ^ b) & 0xFF] ^ (c >> 8)
    return c ^ 0xFFFFFFFF


def mask(c):
    return (((c >> 15) | (c << 17)) + 0xA282EAD8) & 0xFFFFFFFF


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    src, dst = argv[1], argv[2]
    if os.path.exists(dst):
        print(f"refusing: {dst} exists", file=sys.stderr)
        return 1
    data = bytearray(open(src, "rb").read())
    pos, patched = 0, 0
    while pos + 7 <= len(data):
        left = BLOCK - (pos % BLOCK)
        if left < 7:
            pos += left
            continue
        length = struct.unpack_from("<H", data, pos + 4)[0]
        rtype = data[pos + 6]
        if rtype == 0 and length == 0:
            pos += left
            continue
        payload = bytes(data[pos + 7 : pos + 7 + length])
        if OLD in payload:
            payload = payload.replace(OLD, NEW)
            data[pos + 7 : pos + 7 + length] = payload
            struct.pack_into("<I", data, pos, mask(crc32c(bytes([rtype]) + payload)))
            patched += 1
        pos += 7 + length
    if patched == 0:
        print("no record contained the comparator name", file=sys.stderr)
        return 1
    open(dst, "wb").write(data)
    print(f"patched {patched} record(s); sha256 {hashlib.sha256(data).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
