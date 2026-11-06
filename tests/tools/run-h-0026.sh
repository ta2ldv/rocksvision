#!/bin/bash
root="$(cd "$(dirname "$0")/../.." && pwd)"
app="$root/dist/macos-arm64/RocksVision.app"
out="$root/ai-collab/reports/0026/gui/final-d6"
for j in sst wal wood; do
  bash "$root/tests/tools/launch-and-drive.sh" /private/tmp/rocksvision-wal0026/home-final6-$j "$app" "$root/tests/tools/scripts/accept_0026_h_$j.txt" "$out/h-$j" 2>&1 | grep -i "NEW PID\|abort\|GUARD"
  pid=$(pgrep -x rocksvision | tail -1)
  [ -n "$pid" ] && kill "$pid"
  sleep 1
done
