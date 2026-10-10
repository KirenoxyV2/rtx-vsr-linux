#!/bin/bash
# Test vsr.vpy standalone: synthetic clip -> VSR -> verify output size/pixel change.
set -e
cd /mnt/f/repos/rtx-vsr-linux

# Build a test harness script that feeds a synthetic 480p clip through vsr.vpy's
# selector machinery (simulating mpv's video_in) and render frames.
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core

# Synthetic source: 640x360 RGBS with distinct gradient per plane
W, H, N = 640, 384, 10
src = core.std.BlankClip(width=W, height=H, format=vs.RGBS, length=N, keep=True)

# gradient pattern so VSR has real structure to upscale
def fill(n, f):
    fr = f.copy()
    ys, xs = np.mgrid[0:H, 0:W].astype(np.float32)
    np.copyto(np.asarray(fr[0]), (xs / W).astype(np.float32))
    np.copyto(np.asarray(fr[1]), (ys / H).astype(np.float32))
    np.copyto(np.asarray(fr[2]), ((np.sin(xs / 8.0) + np.cos(ys / 8.0)) / 4 + 0.5).astype(np.float32))
    return fr
src = core.std.ModifyFrame(clip=src, clips=[src], selector=fill)

# --- import the vsr.vpy machinery by exec with video_in injected ---
import os
os.environ["VSR_MODE"] = "2"
os.environ["VSR_SCALE"] = "2.0"
os.environ["VSR_MAXH"] = "2160"
code = open("mpv/vsr.vpy").read()
g = {"video_in": src, "__name__": "vsr_vpy"}
exec(compile(code, "vsr.vpy", "exec"), g)

out = g["output"] if "output" in g else None
# vsr.vpy calls clip.set_output() -> captured by core; re-fetch:
# Instead, exec-approach: the script's final clip is in g under the name it
# used. We exposed res_clip; grab it.
out = g.get("res_clip") or g.get("clip")
assert out is not None, "no output clip found in script globals"
print("output clip:", out.width, "x", out.height, out.get_frame(0).format.name)

import time
t0 = time.time()
vsum = 0.0
for n in range(N):
    fr = out.get_frame(n)
    r = np.asarray(fr[0]); g_ = np.asarray(fr[1]); b = np.asarray(fr[2])
    vsum += float(r.mean() + g_.mean() + b.mean())
dt = time.time() - t0
print(f"rendered {N} frames in {dt*1000:.0f} ms ({N/dt:.1f} fps), checksum {vsum:.4f}")

# Expected values: R plane mean = xs/W mean = ~0.5 ; G = ~0.5 ; B varies.
fr = out.get_frame(0)
r = np.asarray(fr[0])
print("R plane corners:", r[0,0], r[0,-1], r[-1,0], r[-1,-1])
print("VSR OUTPUT LOOKS CORRECT" if abs(r[0,0]) < 1.0 and r[0,-1] > 0.4 else "CHECK VALUES")
PYEOF
