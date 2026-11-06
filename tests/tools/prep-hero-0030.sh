#!/bin/bash
# Hero capture prep (0030 section C): copies examples/data/multilevel-advanced-db to /private/tmp/rocksvision-hero (mtimes kept, -p)
# and verifies its hashes against examples/SHA256SUMS (paths relative to examples/).
# usage: bash tests/tools/prep-hero-0030.sh
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
dst=/private/tmp/rocksvision-hero
rm -rf "$dst"
mkdir -p "$dst/examples/data"
cp -Rp "$root/examples/data/multilevel-advanced-db" "$dst/examples/data/"
grep ' data/multilevel-advanced-db/' "$root/examples/SHA256SUMS" > /private/tmp/rocksvision-0030/hero-sums.txt
echo "sums lines: $(wc -l < /private/tmp/rocksvision-0030/hero-sums.txt | tr -d ' ')  files copied: $(find "$dst/examples/data/multilevel-advanced-db" -type f | wc -l | tr -d ' ')"
(cd "$dst/examples" && shasum -a 256 -c /private/tmp/rocksvision-0030/hero-sums.txt | awk '{print $NF}' | sort | uniq -c)
