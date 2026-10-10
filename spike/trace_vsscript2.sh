#!/bin/bash
# Trace WITH the wrapper env to see what VSScript init is missing.
UVPY=/home/kirenoxy/.local/share/uv/python/cpython-3.12.15-linux-x86_64-gnu
VENV=/home/kirenoxy/vsr-venv
SP="$VENV/lib/python3.12/site-packages"
export LD_LIBRARY_PATH="$SP/vapoursynth:$UVPY/lib"
export PYTHONHOME="$VENV"
export PYTHONPATH="$SP"

strace -f -e trace=openat,access,stat,readlink -o /tmp/vs_trace2.txt \
  /opt/mpv-vsr/bin/mpv --vo=null --ao=null --frames=1 \
  --vf=vapoursynth=/mnt/f/repos/rtx-vsr-linux/mpv/vsr.vpy /tmp/vsr_mpv_test.mp4 2>&1 | tail -5
echo "=== python-related probes ==="
grep -E "python|venv|\.so" /tmp/vs_trace2.txt | grep -E "python3\.12|libpython|site-packages|vsr-venv" | tail -25
