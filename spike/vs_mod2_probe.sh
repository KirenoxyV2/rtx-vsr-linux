#!/bin/bash
# Probe 8b: ModifyFrame correct signature (clip=template, clips=[template, src]).
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core
src = core.std.BlankClip(width=64, height=48, format=vs.RGBS, length=3, color=[0.5, 0.2, 0.1])
template = core.std.BlankClip(clip=src, width=128, height=96, format=vs.RGBS)

def sel(f):
    blank, s = f[0], f[1]
    out = blank.copy()                       # writable output-size frame
    up = np.kron(np.asarray(s[0]), np.ones((2, 2), dtype=np.float32))
    np.copyto(np.asarray(out[0]), up)
    np.copyto(np.asarray(out[1]), up * 0.5)
    np.copyto(np.asarray(out[2]), up * 0.25)
    return out

res = core.std.ModifyFrame(clip=template, clips=[template, src], selector=sel)
fr = res.get_frame(1)
print(f"out: {fr.width}x{fr.height} fmt={fr.format.name} r={np.asarray(fr[0])[0,0]:.3f} "
      f"g={np.asarray(fr[1])[0,0]:.3f} b={np.asarray(fr[2])[0,0]:.3f}")
fr2 = res.get_frame(2)
print("frame 2 ok:", fr2.width, fr2.height)
print("MODIFYFRAME SIZE-CHANGE OK")
PYEOF
