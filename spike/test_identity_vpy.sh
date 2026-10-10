#!/bin/bash
# Isolate the VSScript failure: minimal .vpy (no cupy/nvvfx) through mpv.
ls -la /tmp/vsr_mpv_test.mp4 || ffmpeg -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=854x480:rate=30" -t 4 -c:v libx264 -crf 20 -pix_fmt yuv420p /tmp/vsr_mpv_test.mp4

cat > /tmp/identity.vpy <<'EOF'
# minimal identity script: pass video_in through unchanged
video_in.set_output()
EOF

UVPY=/home/kirenoxy/.local/share/uv/python/cpython-3.12.15-linux-x86_64-gnu
VENV=/home/kirenoxy/vsr-venv
SP="$VENV/lib/python3.12/site-packages"
export LD_LIBRARY_PATH="$SP/vapoursynth:$UVPY/lib:${LD_LIBRARY_PATH:-}"
export PYTHONHOME="$VENV"
export PYTHONPATH="$SP"

/opt/mpv-vsr/bin/mpv --vo=null --ao=null --frames=3 \
  --vf=vapoursynth=/tmp/identity.vpy /tmp/vsr_mpv_test.mp4 2>&1 | tail -8
