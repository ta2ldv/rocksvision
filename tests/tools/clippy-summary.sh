#!/usr/bin/env bash
# Runs clippy over all targets (optionally with extra cargo args, e.g. --features harness) and
# prints the distinct warnings ("file:line:col: warning: msg") plus a per-lint count.
# Usage: bash tests/tools/clippy-summary.sh [out-file] [extra cargo args...]
set -euo pipefail
out="${1:-/dev/stdout}"
shift || true
raw="$(cargo clippy --all-targets --locked --target-dir target/developer --message-format json "$@" 2>/dev/null)"
printf '%s\n' "$raw" | python3 -I -c '
import json, sys, collections
seen = {}
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") != "compiler-message":
        continue
    msg = m["message"]
    if msg["level"] not in ("warning", "error") or not msg.get("spans"):
        continue
    code = (msg.get("code") or {}).get("code", "?")
    s = next((s for s in msg["spans"] if s.get("is_primary")), msg["spans"][0])
    key = "%s:%d:%d: %s: %s [%s]" % (s["file_name"], s["line_start"], s["column_start"], msg["level"], msg["message"], code)
    seen[key] = code
for k in sorted(seen):
    print(k)
print("distinct:", len(seen))
for code, n in sorted(collections.Counter(seen.values()).items(), key=lambda x: -x[1]):
    print("  %3d %s" % (n, code))
' > "$out"
