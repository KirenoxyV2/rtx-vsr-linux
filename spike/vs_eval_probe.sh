#!/bin/bash
# Probe 3: FrameEval with f kwarg + size-change output — the exact VSR pattern.
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core
SRC_W, SRC_H = 64, 48
OUT_W, OUT_H = 128, 96
clip = core.std.BlankClip(width=SRC_W, height=SRC_H, format=vs.RGBS, length=5)
out_clip = core.std.BlankClip(width=OUT_W, height=OUT_H, format=vs.RGBS, length=5)

def make_out(n, f):
    src = f[0]                      # VideoFrame of `clip` at frame n
    src_arr = np.asarray(src[0])    # plane 0 as float32 2D
    out = out_clip.get_frame(n).copy()
    # fake "upscale": replicate plane 0 pattern scaled 2x
    big = np.kron(src_arr, np.ones((2, 2), dtype=np.float32))  # 96x128
    np.asarray(out[0])[:] = big
    np.asarray(out[1])[:] = big * 0.5
    np.asarray(out[2])[:] = big * 0.25
    return out

res = core.std.FrameEval(clip=out_clip, eval=make_out, prop_src=[clip])
for n in range(3):
    fr = res.get_frame(n)
    print(f"frame {n}: {fr.width}x{fr.height} p0[0,0]={np.asarray(fr[0])[0,0]:.3f}")

# sequential-order check: are frames requested in order?
order = []
def make_tracked(n, f):
    order.append(n)
    return make_out(n, f)
res2 = core.std.FrameEval(clip=out_clip, eval=make_tracked, prop_src=[clip])
res2.get_frame(0); res2.get_frame(1); res2.get_frame(2)
print("request order:", order)
PYEOF
