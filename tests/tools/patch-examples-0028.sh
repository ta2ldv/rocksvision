#!/bin/bash
# Task 0013 / 0028: register multilevel-advanced-db in tests/examples.rs (DATA list + 27 observed rows).
set -e
f=tests/examples.rs
sed -i '' 's/const DATA: \[&str; 18\]/const DATA: [\&str; 19]/' "$f"
sed -i '' 's/^    "multilevel-db",$/    "multilevel-db",\n    "multilevel-advanced-db",/' "$f"
sed -i '' 's/^            "multilevel-db" => assert_eq!(sst_rows, 7),$/            "multilevel-db" => assert_eq!(sst_rows, 7),\n            "multilevel-advanced-db" => assert_eq!(sst_rows, 27),/' "$f"
git diff --stat -- "$f"
