#!/bin/bash
# vspipe full check: info + render to null.
SP=/home/kirenoxy/vsr-venv/lib/python3.12/site-packages
export LD_LIBRARY_PATH="$SP/vapoursynth:${LD_LIBRARY_PATH:-}"
unset PYTHONHOME PYTHONPATH

cat > /tmp/identity2.vpy <<'EOF'
import vapoursynth as vs
core = vs.core
core.std.BlankClip(width=64, height=48, format=vs.RGB24, length=10).set_output()
EOF

echo "=== vspipe info ==="
"$SP/vapoursynth/vspipe" --info /tmp/identity2.vpy 2>&1 | grep -vE "PYTHONHOME|PYTHONPATH"
echo "=== render ==="
"$SP/vapoursynth/vspipe" /tmp/identity2.vpy /dev/null 2>&1 | grep -vE "PYTHONHOME|PYTHONPATH"
echo "vspipe RC=$?"
