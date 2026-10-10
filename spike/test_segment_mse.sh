#!/bin/bash
# Test the MSE-correct segment transform: fragmented H.264 source ->
# transform_segment -> verify frame-accurate timestamp preservation and
# fragmented output structure.
set -e
cd /mnt/f/repos/rtx-vsr-linux

PY=/home/kirenoxy/vsr-venv/bin/python

# 1. Create a fragmented H.264 source: 640x360, 5s at 30fps, 1s fragments
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "testsrc2=size=640x360:rate=30" -t 5 \
  -c:v libx264 -pix_fmt yuv420p -g 30 \
  -movflags +frag_keyframe+empty_moov+default_base_moof \
  -frag_duration 1000000 /tmp/vsr_seg_src.mp4

$PY - <<'PYEOF'
import sys, time
sys.path.insert(0, "proxy")
import vsr_segment

vsr_segment.gpu_setup(0)

data = open("/tmp/vsr_seg_src.mp4", "rb").read()
t0 = time.time()
out, meta = vsr_segment.transform_segment(data, mode=2, scale=2.0, max_h=1440, gpu=0)
print("meta:", meta)
print(f"elapsed: {time.time()-t0:.2f}s, in {len(data)} B -> out {len(out) if out else 0} B")
open("/tmp/vsr_seg_out.mp4", "wb").write(out)
PYEOF

echo "=== output structure ==="
ffprobe -hide_banner -v error -show_entries "stream=codec_name,width,height" -of default=nw=1 /tmp/vsr_seg_out.mp4
echo "=== pts/dts first+last packets ==="
ffprobe -hide_banner -v error -select_streams v -show_entries "packet=pts,dts,flags" -of csv=p=0 /tmp/vsr_seg_out.mp4 | sed -n '1,3p;$p'
echo "=== is fragmented (moof count)? ==="
grep -c moof <(dd if=/tmp/vsr_seg_out.mp4 bs=1 2>/dev/null) 2>/dev/null || \
  python3 -c "print(open('/tmp/vsr_seg_out.mp4','rb').read().count(b'moof'))"
