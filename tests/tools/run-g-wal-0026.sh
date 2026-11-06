#!/bin/bash
# usage: run-g-wal-0026.sh <pid> <outdir>
pid="$1"; out="$2"
root="$(cd "$(dirname "$0")/../.." && pwd)"; s=/private/tmp/rocksvision-wal0026
run() { env SHOT_DIR="$out/$2" python3 -I "$root/tests/tools/gui-drive.py" --pid "$pid" --set DIR="$1" --set TAG="$2" script "$root/tests/tools/scripts/accept_0026_g_wal.txt" 2>&1 | grep -i "abort\|guard"; }
run "$root/examples/data/multilevel-db" b1-multilevel
run "$root/examples/data/wal-recycled" b1-wal-recycled
run "$s/big-wal" b1-big-wal
run "$s/many-wal" b1-many-wal
run "$s/unknown-cmp" b2-unknown-cmp
echo done
