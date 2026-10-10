#!/usr/bin/env python3
"""Diagnose which VFX effects/modes load on this GPU (arch gate triage).

Tries each effect in a SEPARATE process-safe way (import fresh, catch, report),
because a failed NvVFX_Load can poison the process (double-free on exit).
We run one mode per subprocess invocation and print a one-line JSON verdict.

Usage: python probe_turing.py <n>        (n = 0..6 case index)
"""
import json
import sys


def try_case(idx: int) -> dict:
    import cupy as cp

    dev = cp.cuda.Device(0)
    with dev:
        name = cp.cuda.runtime.getDeviceProperties(0)["name"].decode()
        free, total = cp.cuda.runtime.memGetInfo()

    out = {"case": idx, "gpu": name, "free_mb": free // 2**20}

    if idx == 0:  # BICUBIC (non-AI) upscale
        from nvvfx import VideoSuperRes, QualityLevel

        sr = VideoSuperRes(quality=QualityLevel.BICUBIC, device=0)
        sr.output_width, sr.output_height = 1280, 720
        x = cp.random.rand(3, 360, 640, dtype=cp.float32)
        sr.load()
        res = sr.run(x)
        img = cp.array(cp.from_dlpack(res.image))
        dev.synchronize()
        out.update(effect="VSR/BICUBIC", ok=True, out_shape=str(img.shape))
        return out

    if idx in (1, 2, 3, 4):  # AI upscale LOW..ULTRA
        from nvvfx import VideoSuperRes, QualityLevel

        sr = VideoSuperRes(quality=idx, device=0)
        sr.output_width, sr.output_height = 1280, 720
        x = cp.random.rand(3, 360, 640, dtype=cp.float32)
        sr.load()
        res = sr.run(x)
        img = cp.array(cp.from_dlpack(res.image))
        dev.synchronize()
        out.update(effect=f"VSR/AI-mode{idx}", ok=True, out_shape=str(img.shape))
        return out

    if idx == 5:  # TrueHDR
        from nvvfx import TrueHDR

        t = TrueHDR(device=0)
        x = cp.random.rand(3, 360, 640, dtype=cp.float32)
        t.load()
        res = t.run(x)
        img = cp.array(cp.from_dlpack(res.image))
        dev.synchronize()
        out.update(effect="TrueHDR", ok=True, out_shape=str(img.shape))
        return out

    if idx == 6:  # DENOISE (same-res, mode 8)
        from nvvfx import VideoSuperRes, QualityLevel

        sr = VideoSuperRes(quality=QualityLevel.DENOISE_LOW, device=0)
        sr.output_width, sr.output_height = 640, 360
        x = cp.random.rand(3, 360, 640, dtype=cp.float32)
        sr.load()
        res = sr.run(x)
        img = cp.array(cp.from_dlpack(res.image))
        dev.synchronize()
        out.update(effect="VSR/DENOISE_LOW", ok=True, out_shape=str(img.shape))
        return out

    raise ValueError(f"no case {idx}")


if __name__ == "__main__":
    idx = int(sys.argv[1])
    try:
        r = try_case(idx)
        r["ok"] = True
        r["error"] = None
    except Exception as e:
        r = {"case": idx, "ok": False, "error": f"{type(e).__name__}: {e}"[:300]}
    print("RESULT " + json.dumps(r), file=sys.stderr)
    sys.stderr.flush()
    import os
    os._exit(0)   # skip interpreter teardown: failed-load path double-frees
