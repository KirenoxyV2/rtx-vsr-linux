#!/bin/bash
# Does the wheel's own vspipe work? (same VSScript API as mpv's filter)
UVPY=/home/kirenoxy/.local/share/uv/python/cpython-3.12.15-linux-x86_64-gnu
VENV=/home/kirenoxy/vsr-venv
SP="$VENV/lib/python3.12/site-packages"
export LD_LIBRARY_PATH="$SP/vapoursynth:$UVPY/lib:${LD_LIBRARY_PATH:-}"
export PYTHONHOME="$VENV"
export PYTHONPATH="$SP"

echo "=== vspipe version ==="
"$SP/vapoursynth/vspipe" --version 2>&1 | head -3
echo "=== vspipe identity ==="
cat > /tmp/identity.vpy <<'EOF'
video_in.set_output()
EOF
# vspipe needs a source; use the test clip via core.lsmas? use std.BlankClip instead
cat > /tmp/identity2.vpy <<'EOF'
import vapoursynth as vs
core = vs.core
core.std.BlankClip(width=64, height=48, format=vs.RGB24, length=10).set_output()
EOF
"$SP/vapoursynth/vspipe" /tmp/identity2.vpy - 2>&1 | head -5
echo "RC=$?"
