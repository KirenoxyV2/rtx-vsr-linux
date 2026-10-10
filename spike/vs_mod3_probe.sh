#!/bin/bash
# Probe 8c: ModifyFrame selector signature sel(n, f).
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core
src = core.std.BlankClip(width=64, height=48, format=vs.RGBS, length=3, color=[0.5, 0.2, 0.1])
template = core.std.BlankClip(clip=src, width=128, height=96, format=vs.RGBS)

def sel(n, f):
    blank, s = f[0], f[1]
    out = blank.copy()
    up = np.kron(np.asarray(s[0]), np.ones((2, 2), dtype=np.float32))
    np.copyto(np.asarray(out[0]), up)
    np.copyto(np.asarray(out[1]), up * 0.5)
    np.copyto(np.asarray(out[2]), up * 0.25)
    out.props["FrameNum"] = n
    return out

res = core.std.ModifyFrame(clip=template, clips=[template, src], selector=sel)
for n in (0, 2):
    fr = res.get_frame(n)
    print(f"n={n}: {fr.width}x{fr.height} r={np.asarray(fr[0])[0,0]:.3f} "
          f"props.FrameNum={fr.props.get('FrameNum')}")
print("MODIFYFRAME OK")
PYEOF
