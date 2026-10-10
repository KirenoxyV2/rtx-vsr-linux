#!/bin/bash
# Probe 5: 2D assignment patterns into plane views + FrameEval end-to-end.
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core
out_clip = core.std.BlankClip(width=128, height=96, format=vs.RGBS, length=3)

fr = out_clip.get_frame(0).copy()
arr = np.asarray(fr[0])
big = np.random.rand(96, 128).astype(np.float32)

# Pattern A: direct 2D slice assign
try:
    arr[:] = big
    print("A: arr[:] = big -> OK", np.asarray(fr[0])[0,0])
except Exception as e:
    print("A failed:", type(e).__name__, e)

# Pattern B: np.copyto
fr2 = out_clip.get_frame(0).copy()
arr2 = np.asarray(fr2[0])
try:
    np.copyto(arr2, big * 0.5)
    print("B: np.copyto -> OK", np.asarray(fr2[0])[0,0])
except Exception as e:
    print("B failed:", type(e).__name__, e)

# Pattern C: ellipsis assign
fr3 = out_clip.get_frame(0).copy()
arr3 = np.asarray(fr3[0])
try:
    arr3[...] = big * 0.25
    print("C: arr[...] = -> OK", np.asarray(fr3[0])[0,0])
except Exception as e:
    print("C failed:", type(e).__name__, e)

# FrameEval end-to-end with source READ via explicit get_frame (not prop_src):
src = core.std.BlankClip(width=64, height=48, format=vs.RGBS, length=3, color=[0.5, 0.2, 0.1])
def make_out(n, f):
    sf = src.get_frame(n)          # explicit pull (avoids prop_src view issue)
    r = np.asarray(sf[0]); g = np.asarray(sf[1]); b = np.asarray(sf[2])
    out = out_clip.get_frame(n).copy()
    for p, v in ((0, r), (1, g), (2, b)):
        a = np.asarray(out[p])
        up = np.kron(v, np.ones((2,2), dtype=np.float32))
        np.copyto(a, up)
    return out

res = core.std.FrameEval(clip=out_clip, eval=make_out)
for n in range(3):
    fr = res.get_frame(n)
    print(f"n={n}: {fr.width}x{fr.height} r={np.asarray(fr[0])[0,0]:.3f} g={np.asarray(fr[1])[0,0]:.3f} b={np.asarray(fr[2])[0,0]:.3f}")
print("FRAMEEVAL E2E OK")
PYEOF
