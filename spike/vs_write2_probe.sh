#!/bin/bash
# Probe 4: robust writable plane path — raw pointer + stride-aware numpy wrap.
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import ctypes
import numpy as np
import vapoursynth as vs

core = vs.core
out_clip = core.std.BlankClip(width=128, height=96, format=vs.RGBS, length=5)

fr = out_clip.get_frame(0).copy()
print("frame readonly:", fr.readonly)

# What type does np.asarray(plane) really give us?
p = fr[0]
print("plane type:", type(p))
arr = np.asarray(p)
print("asarray type:", type(arr), arr.dtype, arr.shape, arr.strides, "C:", arr.flags.c_contiguous)

# Try flat-slice write on it
try:
    arr[:] = 0.25
    print("plane-view [:] write OK ->", np.asarray(fr[0])[0,0])
except Exception as e:
    print("plane-view [:] write failed:", type(e).__name__, e)

# Robust path: get_write_ptr + stride
def writable_plane(frame, plane_idx, dtype):
    ptr = frame.get_write_ptr(plane_idx)
    stride = frame.get_stride(plane_idx)
    h, w = frame.height, frame.width
    itemsize = np.dtype(dtype).itemsize
    row_floats = stride // itemsize
    buf = (ctypes.c_char * (stride * h)).from_address(ptr)
    full = np.frombuffer(buf, dtype=dtype).reshape(h, row_floats)
    return full[:, :w], stride

w0, st = writable_plane(fr, 0, np.float32)
print("writable wrap:", w0.shape, w0.strides, "contig:", w0.flags.c_contiguous)
big = np.full((96, 128), 0.75, dtype=np.float32)
w0[:] = big
print("strided write ok ->", np.asarray(fr[0])[0, 0], np.asarray(fr[0])[95, 127])
PYEOF
