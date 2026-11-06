#!/bin/bash
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
bash tests/tools/test-modes.sh target/tester > ai-collab/reports/0026/test-modes-d6.log 2>&1
bash tests/tools/optin-p5-0026.sh > /private/tmp/rocksvision-wal0026/optin-summary-d6.txt 2>&1
echo finished > /private/tmp/rocksvision-wal0026/suite-d6-done.txt
