#!/bin/bash
# usage: bash tests/tools/drive-0030.sh <pid> <item-dir> <script> [--set K=V ...]
# Runs a driver script on own PID; shots go to ai-collab/reports/0030/gui/final-d5/<item-dir>/; prints only aborts/guard/winfo lines.
set -u
pid="$1"; item="$2"; script="$3"; shift 3
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/ai-collab/reports/0030/gui/final-d5/$item"
env SHOT_DIR="$out" python3 -I "$root/tests/tools/gui-drive.py" --pid "$pid" "$@" script "$script" 2>&1 | grep -i "abort\|guard\|window now\|winshot\|clipboard"
echo "done rc=${PIPESTATUS[0]}"
