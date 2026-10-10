#!/bin/bash
# Inspect the VapourSynth Python API surface: how to read/write frame data from Python.
/home/kirenoxy/vsr-venv/bin/python - <<'PYEOF'
import vapoursynth as vs
core = vs.core
print("VS version:", vs.__version__, "core api:", core.version_number(), core.core_version)

# Look for numpy interop on VideoFrame / FrameEval / ModifyFrame
print("\n== VideoFrame attrs ==")
clip = core.std.BlankClip(width=64, height=48, format=vs.RGB24, length=10)
f = clip.get_frame(0)
print([a for a in dir(f) if not a.startswith("__")])

print("\n== std namespace relevant filters ==")
print([a for a in dir(core.std) if any(k in a.lower() for k in ("modify", "frameeval", "eval", "blank", "copy"))])

# ModifyFrame signature
help(core.std.ModifyFrame)
PYEOF
