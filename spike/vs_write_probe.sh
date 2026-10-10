#!/bin/bash
# Probe 2: writable frames + FrameEval with size change (the upscale pattern).
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core
clip = core.std.BlankClip(width=64, height=48, format=vs.RGBS, length=5)
f = clip.get_frame(0)

# 1. Is f.copy() writable, and can np view write into it?
fc = f.copy()
arr = np.asarray(fc[0])
print("copy writable?", not fc.readonly, "| np arr writeable?", arr.flags.writeable)
arr[0, 0] = 0.5
back = np.asarray(fc[0])
print("write stuck?", back[0, 0] == 0.5)

# 2. FrameEval returning a DIFFERENT-SIZE frame built from BlankClip
out_clip = core.std.BlankClip(width=128, height=96, format=vs.RGBS, length=5)

def make_out(n):
    base = out_clip.get_frame(n).copy()   # writable, correct size
    for p in range(3):
        a = np.asarray(base[p])
        a[:] = 0.25 + 0.01 * n
    return base

res = core.std.FrameEval(
    clip=out_clip,
    eval=make_out,
    prop_src=[clip],
)
# render a few frames
for n in range(3):
    fr = res.get_frame(n)
    v = np.asarray(fr[0])[0, 0]
    print(f"frame {n}: value={v} size={fr.width}x{fr.height} ok")

# 3. Does get_frame of a FrameEval clip trigger the callback per frame?
calls = {"n": 0}
def make_out2(n):
    calls["n"] += 1
    return make_out(n)
res2 = core.std.FrameEval(clip=out_clip, eval=make_out2, prop_src=[clip])
res2.get_frame(0); res2.get_frame(1)
print("callback invoked:", calls["n"], "times for 2 frames")

# 4. numpy -> plane write via get_write_ptr (in case plane views are read-only)
import ctypes
fr = out_clip.get_frame(0).copy()
ptr = fr.get_write_ptr(0)
stride = fr.get_stride(0)
a = np.ctypeslib.as_array(ctypes.cast(ptr, ctypes.POINTER(np.float32)), shape=(96 * stride // 4,))
a[0] = 0.75
print("manual ptr write ok; read back:", np.asarray(fr[0])[0, 0])
PYEOF
