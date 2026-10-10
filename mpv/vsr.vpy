# vsr.vpy — RTX Video Super Resolution as a VapourSynth script for mpv.
#
# Works with any player/binary built with vf_vapoursynth (mpv, mpv.net, VSEdit).
#
# mpv usage:
#   mpv --vf=vapoursynth=<path>/vsr.vpy <file>
#   # or persistent, in mpv.conf:
#   #   vf=vapoursynth=<path>/vsr.vpy
#   # toggle at runtime by adding/removing the vf entry via scripting.
#
# Config via environment variables (inherited by mpv):
#   VSR_MODE   1=LOW 2=MEDIUM 3=HIGH 4=ULTRA (default 2)
#   VSR_SCALE  output scale factor (default 2.0)
#   VSR_MAXH   cap output height, sources >= cap pass through (default 2160)
#   VSR_GPU    CUDA device (default 0)
#
# Pipeline per frame: VS plane views -> 3-plane stack (CHW) cupy tensor ->
# nvvfx VideoSuperRes.run -> deep-copy result -> numpy -> write to output-size
# frame planes. The engine is loaded once; warmup once at script start.

import os

import numpy as np

# ---------------------------------------------------------------- inputs
MODE = int(os.environ.get("VSR_MODE", "2"))
SCALE = float(os.environ.get("VSR_SCALE", "2.0"))
MAX_H = int(os.environ.get("VSR_MAXH", "2160"))
GPU = int(os.environ.get("VSR_GPU", "0"))

# `video_in` is injected by mpv's vapoursynth vf when used through mpv.
# For standalone .vpy use (vspipe etc.) it would be absent — fall back to a
# file arg handled below.
try:
    clip = video_in  # noqa: F821
    HAVE_MPV = True
except NameError:
    import sys
    import vapoursynth as vs
    core = vs.core
    clip = core.std.BlankClip(width=64, height=48, format=vs.RGBS, length=1)
    HAVE_MPV = False
    if len(sys.argv) > 1:
        clip = core.bs.VideoSource(sys.argv[1])
    else:
        raise SystemExit("vsr.vpy: no mpv video_in and no source file argument")

import vapoursynth as vs

core = vs.core

# ---------------------------------------------------------------- policy
if clip.height >= MAX_H or SCALE <= 1.0:
    # Source already at/above cap (or no upscale wanted): zero AI work.
    clip.set_output()
    raise SystemExit(0)


def _align16(n):
    return max(16, int(round(n / 16.0)) * 16)


OUT_W = _align16(clip.width * SCALE)
OUT_H = _align16(clip.height * SCALE)
if OUT_H > MAX_H:
    eff = MAX_H / clip.height
    OUT_H = _align16(MAX_H)
    OUT_W = _align16(clip.width * eff)

# ---------------------------------------------------------------- model
import cupy as cp
from nvvfx import VideoSuperRes

_sr = VideoSuperRes(quality=MODE, device=GPU)
_sr.output_width = OUT_W
_sr.output_height = OUT_H
_sr.load()
_dev = cp.cuda.Device(GPU)

# Warmup: first run initializes the engine/allocations. Hold result alive and
# deep-copy (DLPack output is effect-owned; freeing it mid-transfer corrupts
# the CUDA context — see repo DESIGN §9a).
_w = cp.random.rand(3, clip.height, clip.width, dtype=cp.float32)
_res = _sr.run(_w)
_img = cp.array(cp.from_dlpack(_res.image))
_dev.synchronize()
del _res, _img, _w

# ---------------------------------------------------------------- to model fmt
# mpv hands us RGB or YUV in the display-ish format; VSR wants RGB float32
# CHW. If mpv gave YUV, convert with resize.Bicubic before processing.
if clip.format.color_family == vs.YUV:
    clip = core.resize.Bicubic(clip, format=vs.RGBS, matrix_in_s="709")

SRC_H = clip.height
SRC_W = clip.width

# ---------------------------------------------------------------- selector
_template = core.std.BlankClip(clip=clip, width=OUT_W, height=OUT_H,
                               format=vs.RGBS)

# Preallocate pinned host staging? VS gives per-frame temp frames; we reuse
# output frames from the template clip (each get_frame is cheap; the model
# dominates). Keep per-call allocs minimal by caching the template frame.
_frame_cache = {}


def sel(n, f):
    src = f[1]

    # stack planes: (3, SRC_H, SRC_W) float32
    planes = [np.asarray(src[p]) for p in range(3)]
    x = cp.asarray(np.stack(planes))  # H2D

    res = _sr.run(x)
    out = cp.array(cp.from_dlpack(res.image))  # deep copy (use-after-free guard)
    del res
    _dev.synchronize()
    h = out.get()
    del out

    frame = f[0].copy()
    # copy each upscaled plane into the writable output frame
    for p in range(3):
        np.copyto(np.asarray(frame[p]), h[p])
    return frame


res_clip = core.std.ModifyFrame(clip=_template, clips=[_template, clip],
                                selector=sel)
res_clip.set_output()
