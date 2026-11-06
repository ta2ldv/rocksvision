#!/bin/bash
# Scratch WAL fixtures for the 0026 acceptance run, all under /private/tmp/rocksvision-wal0026/.
# Idempotent: every folder is rebuilt, the (slow) big WAL is generated once and cached in gen/.
# Folders: no-wal corrupt-wal big-wal many-wal multi-cf unknown-cmp   (+ home-* made by the GUI runs)
# Limits (src/limits.rs): WAL_SUMMARY_BYTES = 16 MiB per WAL, WAL_SUMMARY_FILES = 32 per folder.
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
s=/private/tmp/rocksvision-wal0026
src="$root/examples/data/multilevel-db"
mkdir -p "$s/gen"
cd "$s"
rm -rf no-wal corrupt-wal big-wal many-wal multi-cf unknown-cmp

# (a) no WAL: multilevel-db without its .log
cp -R "$src" no-wal
rm no-wal/*.log

# (b) corrupt WAL: one byte of 000023.log (108 bytes) XOR 0xFF at offset 54 (mid file)
cp -R "$src" corrupt-wal
rm corrupt-wal/000023.log
python3 -I "$root/tests/tools/flip-byte.py" "$src/000023.log" corrupt-wal/000023.log 54

# (c) big WAL: a real engine-written WAL of 17 MiB (> WAL_SUMMARY_BYTES 16 MiB) from
#     `fixture-gen large-wal` (single default CF), renamed over 000023.log of a multilevel-db copy.
if [ ! -f gen/large-wal.log ]; then
  "$root/tests/fixture-gen/target/release/fixture-gen" large-wal "$s/gen" --mb 17
fi
cp -R "$src" big-wal
cp gen/large-wal.log big-wal/000023.log

# (d) many WALs: 40 copies of the real WAL as 000100.log .. 000139.log (+ the real 000023.log).
#     Their numbers do not match the MANIFEST log numbers; fine for the "+N older" overflow test.
cp -R "$src" many-wal
for n in $(seq 100 139); do cp "$src/000023.log" "many-wal/000$n.log"; done

# (e) multi-CF WAL: examples/data/wal-recycled (CURRENT+MANIFEST+000011.log+000014.log;
#     000014.log writes to CF 0 and 1, and is Corrupt at its recycled tail per the D0 report).
cp -R "$root/examples/data/wal-recycled" multi-cf

# (f) unknown comparator: multilevel-db with the MANIFEST comparator renamed (same length, CRC fixed)
cp -R "$src" unknown-cmp
rm unknown-cmp/MANIFEST-000005
python3 -I "$root/tests/tools/rename-comparator-0026.py" "$src/MANIFEST-000005" unknown-cmp/MANIFEST-000005

# (g) multi-cf-done: clean DB folder, one 604 B WAL with writes to CF 0,1,2, state Done expected.
#     Needs the fixture-gen debug binary built by:
#     env LIBCLANG_PATH=... DYLD_FALLBACK_LIBRARY_PATH=... cargo run --manifest-path tests/fixture-gen/Cargo.toml --target-dir target/tester -- multi-cf-wal <dir>
if [ -x "$root/target/tester/debug/fixture-gen" ]; then
  "$root/target/tester/debug/fixture-gen" multi-cf-wal "$s/multi-cf-done" > /dev/null
fi

# (h) many-sst: 600 tiny L0 SSTs, one flush each (density bars / truncation note), about 3 s.
if [ -x "$root/target/tester/debug/fixture-gen" ]; then
  "$root/target/tester/debug/fixture-gen" many-sst "$s/many-sst" --files 600
fi

echo "--- paths and sha256"
for f in no-wal corrupt-wal/000023.log big-wal/000023.log many-wal/000100.log multi-cf/000014.log multi-cf/000011.log unknown-cmp/MANIFEST-000005; do
  if [ -d "$f" ]; then echo "$s/$f  (dir, files: $(ls "$f" | wc -l | tr -d ' '), logs: $(ls "$f"/*.log 2>/dev/null | wc -l | tr -d ' '))"
  else echo "$s/$f  $(wc -c < "$s/$f" | tr -d ' ') B  $(shasum -a 256 "$s/$f" | cut -d' ' -f1)"; fi
done
echo "no-wal logs: $(ls no-wal/*.log 2>/dev/null | wc -l | tr -d ' ')   many-wal logs: $(ls many-wal/*.log | wc -l | tr -d ' ')"
