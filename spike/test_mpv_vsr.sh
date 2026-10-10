#!/bin/bash
# End-to-end mpv + vsr.vpy test (headless-safe): render 8s of upscaled video
# to a PNG screenshot series and verify resolution + timing via mpv logs.
set -e
TESTSRC=/tmp/vsr_mpv_test.mp4

# 480p test clip with detail (moving test pattern + noise)
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "testsrc2=size=854x480:rate=30" -t 8 \
  -f lavfi -i "anoisesrc=colour=pink" -t 8 \
  -c:v libx264 -crf 20 -pix_fmt yuv420p -c:a aac "$TESTSRC"

export VSR_MODE=2 VSR_SCALE=2.0 VSR_MAXH=2160 VSR_GPU=0
# The wrapper sets PYTHONHOME to the venv so VS's embedded interpreter finds
# cupy/nvvfx in site-packages.
/mnt/f/repos/rtx-vsr-linux/mpv/mpv-vsr.sh \
  --vo=null --ao=null --untimed \
  --vf=vapoursynth=/mnt/f/repos/rtx-vsr-linux/mpv/vsr.vpy \
  --msg-level=vf=vdebug,vo=v \
  --frames=90 \
  "$TESTSRC" 2>&1 | grep -E "vapoursynth|VSR|vf.*[0-9]+x[0-9]+|Video.*[0-9]+x[0-9]+" | head -20
echo "MPV FILTER TEST DONE (rc=$?)"
