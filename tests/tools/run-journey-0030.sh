#!/bin/bash
# usage: bash tests/tools/run-journey-0030.sh <pid> ; Wooden + Amiga Tool Style journeys (own PID)
pid="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
for t in "wooden 474 340" "amigatool 500 296"; do
  set -- $t
  bash "$root/tests/tools/drive-0030.sh" "$pid" themes "$root/tests/tools/scripts/d2_journey.txt" --set N="$1" --set TX="$2" --set TY="$3" --set DIR="$root/examples/data/multilevel-advanced-db" | tail -1
done
