#!/bin/bash
# Raw mpv+VSR run: no grep, full log tail.
export VSR_MODE=2 VSR_SCALE=2.0 VSR_MAXH=2160 VSR_GPU=0
/mnt/f/repos/rtx-vsr-linux/mpv/mpv-vsr.sh \
  --vo=null --ao=null --untimed \
  --vf=vapoursynth=/mnt/f/repos/rtx-vsr-linux/mpv/vsr.vpy \
  --frames=30 \
  /tmp/vsr_mpv_test.mp4 2>&1 | tail -30
echo "RC=$?"
