#!/bin/bash
# Diagnose VSScript init: which python does libvsscript look for?
ls /home/kirenoxy/vsr-venv/bin/ | head
ls /home/kirenoxy/vsr-venv/lib/python3.12/site-packages/vapoursynth/ | head -20
echo "== libvsscript deps =="
ldd /home/kirenoxy/vsr-venv/lib/python3.12/site-packages/vapoursynth/libvsscript.so | grep -E "python|not found"
echo "== strings: python hints =="
strings /home/kirenoxy/vsr-venv/lib/python3.12/site-packages/vapoursynth/libvsscript.so | grep -iE "^python3|libpython|PYTHONHOME|pythonhome" | sort -u | head
echo "== does venv have libpython? =="
find /home/kirenoxy/vsr-venv -name "libpython*" 2>/dev/null | head
find /home/kirenoxy/.local/share/uv/python/cpython-3.12.15-linux-x86_64-gnu -name "libpython3.12*" 2>/dev/null | head
