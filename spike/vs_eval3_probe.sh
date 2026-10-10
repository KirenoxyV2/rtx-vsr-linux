#!/bin/bash
# Probe 6: FrameEval callback signature: f as positional list?
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core
out_clip = core.std.BlankClip(width=128, height=96, format=vs.RGBS, length=3)
src = core.std.BlankClip(width=64, height=48, format=vs.RGBS, length=3, color=[0.5, 0.2, 0.1])

def make_out(n, f):
    sf = f[0]                       # positional list of source frames
    r = np.asarray(sf[0])
    out = out_clip.get_frame(n).copy()
    up = np.kron(r, np.ones((2, 2), dtype=np.float32))
    np.copyto(np.asarray(out[0]), up)
    return out

res = core.std.FrameEval(clip=out_clip, eval=make_out, prop_src=[src])
fr = res.get_frame(1)
print("E2E:", fr.width, "x", fr.height, "r value:", np.asarray(fr[0])[0, 0])

# double-check: src plane read is as expected
print("src r:", np.asarray(src.get_frame(0)[0])[0,0])
print("FRAMEEVAL LIST-ARG OK")
PYEOF
