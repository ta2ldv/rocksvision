#!/bin/bash
# usage: bash tests/tools/run-wal-journeys-0026.sh <pid> <outdir> ; opens each folder in the running app (own PID), shots per folder
set -u
pid="$1"; out="$2"
root="$(cd "$(dirname "$0")/../.." && pwd)"
s=/private/tmp/rocksvision-wal0026
run() {
  env SHOT_DIR="$out/$2" python3 -I "$root/tests/tools/gui-drive.py" --pid "$pid" --set DIR="$1" --set TAG="$2" script "$root/tests/tools/scripts/accept_0026_wal_open.txt" 2>&1 | grep -i "abort\|guard"
}
run "$root/tests/fixtures/db-checkpoint" j4-checkpoint
run "$s/no-wal" j4-no-wal
run "$s/corrupt-wal" j4-corrupt-wal
run "$s/big-wal" j4-big-wal
run "$s/many-wal" j4-many-wal
run "$s/multi-cf-sst" j4-multi-cf-sst
run "$root/examples/data/wal-recycled" j4-wal-recycled
echo done
