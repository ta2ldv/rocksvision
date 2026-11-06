#!/bin/bash
# four modes then the five opt-in entries (sequential: one cargo target dir)
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
bash tests/tools/test-modes.sh target/tester > ai-collab/reports/0026/test-modes-d5.log 2>&1
bash tests/tools/optin-p5-0026.sh > /private/tmp/rocksvision-wal0026/optin-summary.txt 2>&1
echo finished > /private/tmp/rocksvision-wal0026/suite-done.txt
