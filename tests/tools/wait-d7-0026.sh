#!/bin/bash
for i in $(seq 1 100); do [ -f /private/tmp/rocksvision-wal0026/suite-d7-done.txt ] && exit 0; sleep 3; done
exit 1
