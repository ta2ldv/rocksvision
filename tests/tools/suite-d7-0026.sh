#!/bin/bash
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
bash tests/tools/test-modes.sh target/tester > ai-collab/reports/0026/test-modes-d7.log 2>&1
echo finished > /private/tmp/rocksvision-wal0026/suite-d7-done.txt
