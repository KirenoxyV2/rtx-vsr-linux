#!/bin/bash
# MSE-correctness test for the proxy: H.264 fMP4 segment -> /transform ->
# verify output is decodable fMP4 (init segment + media segments with valid
# timestamps) that could be appended to a video/mp4 SourceBuffer.
set -e
cd /mnt/f/repos/rtx-vsr-linux

PY=/home/kirenoxy/vsr-venv/bin/python

# 1. Create a synthetic H.264 fMP4 test source (10s, 640x360, 30fps)
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "testsrc2=size=640x360:rate=30" -t 10 \
  -c:v libx264 -pix_fmt yuv420p -movflags +frag_keyframe+empty_moov+default_base_moof \
  -frag_duration 1 /tmp/vsr_test_src.mp4

# 2. Run proxy as a library call (no HTTP needed for correctness check)
$PY - <<'PYEOF'
import os, sys, time
os.chdir("/mnt/f/repos/rtx-vsr-linux")
sys.path.insert(0, "proxy")
os.environ.setdefault("VSR_MAXH", "1440")
import vsr_proxy

vsr_proxy.POLICY["max_out_h"] = 1440
vsr_proxy._gpu_setup(0)
t0 = time.time()
data, meta = vsr_proxy.upscale_clip("/tmp/vsr_test_src.mp4", mode=2, scale=2.0, gpu_index=0)
open("/tmp/vsr_test_out.mp4", "wb").write(data)
print("meta:", meta)
print(f"elapsed {time.time()-t0:.2f}s, out bytes {len(data)}")
PYEOF

# 3. Verify output structure with ffprobe
echo "=== output stream ==="
ffprobe -hide_banner -v error -show_entries "stream=codec_name,width,height,avg_frame_rate,nb_frames" -of default=nw=1 /tmp/vsr_test_out.mp4
echo "=== packet timestamps (first 8) ==="
ffprobe -hide_banner -v error -select_streams v -show_entries "packet=pts,dts,duration,flags" -of csv=p=0 /tmp/vsr_test_out.mp4 | head -8
echo "=== frag structure? ==="
ffprobe -hide_banner -v error -show_entries "format=format_name" -of default=nw=1 /tmp/vsr_test_out.mp4
