#!/usr/bin/env python3
"""Check mpv/vsr.vpy outside mpv: colour-math correctness + throughput.

Runs the script the way mpv does (exec with `video_in` / `user_data` injected)
and compares its all-GPU colour path against a zimg reference built from the
same model:   zimg YUV->RGBS  ->  VideoSuperRes  ->  zimg RGBS->YUV.
Any matrix / range / chroma-siting mistake in the CUDA kernels shows up as a
large per-plane error or a non-zero best-fit shift.

Usage:  ~/vsr-venv/bin/python spike/test_vpy.py [video] [user-data]
        (needs the `vapoursynth` wheel in that venv; video defaults to the
        repo's 720p sample, user-data to "mode=2,maxw=2560,maxh=1440")
"""
import os
import sys
import time

import av
import numpy as np
import vapoursynth as vs

HERE = os.path.dirname(os.path.abspath(__file__))
VPY = os.path.join(HERE, "..", "mpv", "vsr.vpy")
core = vs.core


def source_clip(path, n):
    """First n frames of `path` as a YUV420P8 clip with mpv-style props."""
    frames = []
    with av.open(path) as c:
        for fr in c.decode(video=0):
            frames.append(fr.reformat(format="yuv420p").to_ndarray())
            if len(frames) == n:
                break
    h = frames[0].shape[0] * 2 // 3
    w = frames[0].shape[1]
    blank = core.std.BlankClip(width=w, height=h, format=vs.YUV420P8,
                               length=len(frames), fpsnum=30, fpsden=1)

    def fill(n, f):
        out = f.copy()
        a = frames[n]
        np.copyto(np.asarray(out[0]), a[:h])
        np.copyto(np.asarray(out[1]), a[h:h + h // 4].reshape(h // 2, w // 2))
        np.copyto(np.asarray(out[2]), a[h + h // 4:].reshape(h // 2, w // 2))
        out.props["_Matrix"] = 1
        out.props["_ColorRange"] = 1      # what mpv sets (1 = limited)
        out.props["_ChromaLocation"] = 0
        return out

    return core.std.ModifyFrame(blank, clips=[blank], selector=fill)


def run_vpy(clip, user_data):
    g = {"video_in": clip, "user_data": user_data, "__file__": VPY,
         "__name__": "__vapoursynth__"}
    exec(compile(open(VPY).read(), VPY, "exec"), g)
    return g


def reference(clip, g, n):
    """Frame n through zimg colour conversion around the same model."""
    import cupy as cp

    eng = g["_state"](clip.format, clip.width, clip.height)
    rgb = core.resize.Bicubic(clip, format=vs.RGBS, matrix_in_s="709",
                              range_in_s="limited", chromaloc_in_s="left")
    f = rgb.get_frame(n)
    x = cp.asarray(np.stack([np.asarray(f[p]) for p in range(3)]))
    with eng.lock:
        res = eng.sr.run(cp.clip(x, 0, 1))
        up = cp.array(cp.from_dlpack(res.image))
        del res
    host = cp.asnumpy(cp.clip(up, 0, 1))
    blank = core.std.BlankClip(width=host.shape[2], height=host.shape[1],
                               format=vs.RGBS, length=1)

    def fill(n, f):
        out = f.copy()
        for p in range(3):
            np.copyto(np.asarray(out[p]), host[p])
        return out

    up_clip = core.std.ModifyFrame(blank, clips=[blank], selector=fill)
    # Bilinear-free reference: zimg's own (bicubic) chroma downsampling.
    yuv = core.resize.Bicubic(up_clip, format=g["eng_fmt"], matrix_s="709",
                              range_s="limited", chromaloc_s="left")
    return yuv.get_frame(0)


def best_shift(a, b, r=2):
    a = a.astype(np.float32)
    b = b.astype(np.float32)
    m = 8
    return min((float(np.abs(np.roll(a, (dy, dx), (0, 1))[m:-m, m:-m]
                             - b[m:-m, m:-m]).mean()), dx, dy)
               for dx in range(-r, r + 1) for dy in range(-r, r + 1))


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "sample_720p.mp4")
    user_data = sys.argv[2] if len(sys.argv) > 2 else "mode=2,maxw=2560,maxh=1440"
    n = 60
    clip = source_clip(path, n)
    g = run_vpy(clip, user_data)
    out = g["res_clip"]
    print(f"in  {clip.width}x{clip.height} {clip.format.name}")
    print(f"out {out.width}x{out.height} {out.format.name}  active={g['ACTIVE']}")
    if not g["ACTIVE"]:
        print("passthrough — nothing to check")
        return 0

    g["eng_fmt"] = out.format.id
    scale = 1 << (out.format.bits_per_sample - 8)
    ok = True
    for idx in (0, n // 2):
        got = out.get_frame(idx)
        ref = reference(clip, g, idx)
        for p, name in enumerate("YUV"):
            err, dx, dy = best_shift(np.asarray(got[p]), np.asarray(ref[p]))
            d = (np.asarray(got[p]).astype(np.float32)
                 - np.asarray(ref[p]).astype(np.float32)) / scale
            plain, bias = float(np.abs(d).mean()), float(d.mean())
            good = (dx, dy) == (0, 0) and plain < 1.0 and abs(bias) < 0.25
            ok &= good
            print(f"  frame {idx:2d} {name}: mean|diff| {plain:.3f}, bias {bias:+.3f} "
                  f"(8-bit steps), best shift ({dx},{dy})  {'ok' if good else 'MISMATCH'}")
        for k in ("_DurationNum", "_DurationDen", "_Matrix"):
            if k not in got.props:
                ok = False
                print(f"  frame {idx}: prop {k} missing on output  MISMATCH")

    t0 = time.time()
    for i in range(n):
        out.get_frame(i)
    dt = time.time() - t0
    print(f"throughput: {n} frames in {dt * 1000:.0f} ms = {n / dt:.1f} fps "
          f"({dt / n * 1000:.1f} ms/frame, decode excluded)")
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    rc = main()
    sys.stdout.flush()
    os._exit(rc)   # nvvfx teardown can double-free; skip interpreter exit
