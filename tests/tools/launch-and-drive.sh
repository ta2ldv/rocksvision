#!/bin/bash
# Launches the packaged app with a given HOME (open -n), finds the NEW rocksvision PID (the one not
# running before) and runs the GUI driver script on exactly that PID.
# usage: bash tests/tools/launch-and-drive.sh <home-dir> <RocksVision.app> <driver-script> <SHOT_DIR>
set -u
home="$1"; app="$2"; script="$3"; shots="$4"
root="$(cd "$(dirname "$0")/../.." && pwd)"
before="$(pgrep -x rocksvision | sort | tr '\n' ' ')"
mkdir -p "$home"
open --env HOME="$home" -n "$app"
pid=""
for _ in $(seq 1 50); do
  for p in $(pgrep -x rocksvision); do
    case " $before " in *" $p "*) ;; *) pid="$p" ;; esac
  done
  [ -n "$pid" ] && break
  sleep 0.1
done
[ -n "$pid" ] || { echo "no new rocksvision process found"; exit 1; }
echo "NEW PID $pid started $(date +%H:%M:%S)"
env SHOT_DIR="$shots" python3 -I "$root/tests/tools/gui-drive.py" --pid "$pid" script "$script"
