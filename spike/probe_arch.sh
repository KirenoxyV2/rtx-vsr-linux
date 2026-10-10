#!/bin/bash
# Count per-arch cubin targets embedded in the bundled VSR model libs.
L=/home/kirenoxy/vsr-venv/lib/python3.12/site-packages/nvvfx/libs
for f in "$L"/libnvidia-ngx-vsr.so.1.8.3 "$L"/libnvVFXVideoSuperRes.so "$L"/libVideoFX.so "$L"/libnvngxruntime.so; do
  echo "## $(basename "$f")"
  for s in sm_50 sm_60 sm_70 sm_75 sm_80 sm_86 sm_89 sm_90 sm_100 sm_120 sm_121; do
    n=$(strings -a "$f" | grep -c "target $s")
    [ "$n" -gt 0 ] && echo "  $s : $n"
  done
done
