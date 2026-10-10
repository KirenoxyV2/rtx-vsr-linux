#!/bin/bash
# Probe 7: what is `f` in FrameEval, and how to read planes of READONLY frames.
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core
src = core.std.BlankClip(width=64, height=48, format=vs.RGBS, length=3, color=[0.5, 0.2, 0.1])
out_clip = core.std.BlankClip(width=128, height=96, format=vs.RGBS, length=3)

# 1. readonly frame plane read via np.asarray (no copy)
sf = src.get_frame(0)
print("readonly:", sf.readonly)
try:
    a = np.asarray(sf[0])
    print("readonly plane np.asarray OK:", a.shape, a[0,0])
except Exception as e:
    print("readonly plane np.asarray FAILED:", type(e).__name__, e)

# 2. inspect what FrameEval passes
def probe_cb(n, f):
    print("n=", n, "| f type:", type(f), "| len:", (len(f) if hasattr(f, "__len__") else "?"))
    fr = f[0] if isinstance(f, (list, tuple)) else f
    print("   element type:", type(fr))
    try:
        pv = fr[0]
        print("   plane type:", type(pv))
        arr = np.asarray(pv)
        print("   np.asarray(plane):", arr.shape, arr[0,0])
    except Exception as e:
        print("   plane read FAILED:", type(e).__name__, e)
    return out_clip.get_frame(n).copy()

res = core.std.FrameEval(clip=out_clip, eval=probe_cb, prop_src=[src])
res.get_frame(0)
print("PROBE7 DONE")
PYEOF
