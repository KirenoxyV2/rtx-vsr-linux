#!/bin/bash
# Probe VapourSynth <-> numpy interop primitives needed for the VSR .vpy.
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import numpy as np
import vapoursynth as vs

core = vs.core
clip = core.std.BlankClip(width=64, height=48, format=vs.RGBS, length=5)

f = clip.get_frame(0)

# 1. Does np.asarray(frame[plane]) work (buffer protocol)?
try:
    p0 = np.asarray(f[0])
    print("np.asarray(f[0]) ->", p0.shape, p0.dtype, "strides:", p0.strides)
except Exception as e:
    print("np.asarray(f[0]) FAILED:", e)

# 2. frame.get_read_ptr + get_stride manual access
try:
    print("read_ptr:", f[0].get_read_ptr(False) if hasattr(f[0], "get_read_ptr") else f.get_read_ptr(0))
    print("stride:", f.get_stride(0))
    print("frame size:", f.width, f.height)
except Exception as e:
    print("ptr access FAILED:", e)

# 3. Can we build a frame from numpy? Look for a helper on the module.
print("\nmodule-level helpers:",
      [a for a in dir(vs) if "frame" in a.lower() or "numpy" in a.lower() or "array" in a.lower()])

# 4. std.ModifyFrame behavior with a callback that returns a new-size frame?
res = core.std.ModifyFrame(clip=clip, format=None,
                           clips=[clip], selector=None)
print("ModifyFrame attr:", res)

# 5. What about core.std.FrameEval signature?
import inspect
print("\nFrameEval:", core.std.FrameEval.__doc__[:400] if core.std.FrameEval.__doc__ else "no doc")

# 6. Check for vs.core.std.CopyFrame and friends
print("\nstd functions with 'frame':", [a for a in dir(core.std) if "frame" in a.lower()])
PYEOF
