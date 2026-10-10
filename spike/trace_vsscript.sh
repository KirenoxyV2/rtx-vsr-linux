#!/bin/bash
# Trace: what does libvsscript.so dlopen at init?
strace -f -e trace=openat,access,stat,readlink -o /tmp/vs_trace.txt \
  /opt/mpv-vsr/bin/mpv --vo=null --ao=null --frames=1 \
  --vf=vapoursynth=/mnt/f/repos/rtx-vsr-linux/mpv/vsr.vpy /tmp/vsr_mpv_test.mp4 2>/dev/null || true
grep -E "python|venv|vapoursynth|vsscript" /tmp/vs_trace.txt | grep -vE "ENOENT.*(locale|gconv)" | tail -30
