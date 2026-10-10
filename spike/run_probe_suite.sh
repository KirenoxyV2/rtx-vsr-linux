#!/bin/bash
# Run the full turing probe suite (one case per process) and collect results.
cd /mnt/f/repos/rtx-vsr-linux || exit 1
for i in 0 1 2 3 4 5 6; do
  timeout 600 /home/kirenoxy/vsr-venv/bin/python spike/probe_turing.py "$i" 2>&1 | grep RESULT
done
echo "SUITE DONE"
