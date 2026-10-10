#!/bin/bash
# Install VapourSynth (pip wheel bundles libvapoursynth-core) into the vsr venv.
set -x
/home/kirenoxy/.local/bin/uv pip install --python /home/kirenoxy/vsr-venv vapoursynth 2>&1 | tail -2
ls /home/kirenoxy/vsr-venv/lib/python3.12/site-packages/ | grep -i vapour
find /home/kirenoxy/vsr-venv -name "libvapoursynth*" 2>/dev/null | head
find /home/kirenoxy/vsr-venv -name "vapoursynth*" -maxdepth 6 2>/dev/null | head
