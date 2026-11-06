#!/bin/bash
# usage: bash tests/tools/run-readme-0030.sh <pid> ; README walkthroughs 1-3 + F3/Help check on own PID, shots to final-d3/readme
pid="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
d="$root/tests/tools/drive-0030.sh"; s="$root/tests/tools/scripts"
bash $d $pid readme $s/c_w1a.txt --set DIR=$root/examples/data/multilevel-advanced-db | tail -1
bash $d $pid readme $s/c_w1b.txt | tail -1
bash $d $pid readme $s/c_w2a.txt --set DIR=$root | tail -1
bash $d $pid readme $s/c_w2b.txt | tail -1
bash $d $pid readme $s/c_w2c.txt | tail -1
bash $d $pid readme $s/c_w3a.txt --set DIR=$root | tail -1
bash $d $pid readme $s/c_w3b.txt | tail -1
bash $d $pid readme $s/c_w3c.txt | tail -1
echo all-done
