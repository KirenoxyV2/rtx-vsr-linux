#!/usr/bin/env python3
"""M1 spike: does nvidia-vfx (VFX SDK) run on consumer Ada, and at what cost?

Measures ms/frame for the VideoSuperRes effect across quality modes and
resolutions, reports VRAM footprint, and fails soft (records OOM) so a single
run characterizes what fits in available VRAM.

Usage:
    python bench_vsr.py [gpu_index] [--quick]

--quick: only 720p->1440p at LOW/MEDIUM for a fast smoke test.
"""
import argparse
import sys
import time

import cupy as cp


def fmt_mb(nbytes: int) -> str:
    return f"{nbytes / (1024 * 1024):.0f} MB"


def bench(gpu: int, mode: int, in_w: int, in_h: int, out_w: int, out_h: int,
          iters: int = 15) -> dict:
    from nvvfx import VideoSuperRes

    dev = cp.cuda.Device(gpu)
    with dev:
        free_before, _total = cp.cuda.runtime.memGetInfo()
        # channels-first RGB float32 in [0,1], per nvidia-vfx quickstart
        x = cp.random.rand(3, in_h, in_w, dtype=cp.float32)

        sr = VideoSuperRes(quality=mode, device=gpu)
        sr.output_width = out_w
        sr.output_height = out_h
        sr.load()

        # warmup (engine init, first-frame allocations)
        last = None
        for _ in range(3):
            last = cp.from_dlpack(sr.run(x).image)
        dev.synchronize()

        t0 = time.perf_counter()
        for _ in range(iters):
            last = cp.from_dlpack(sr.run(x).image)
            dev.synchronize()
        ms = (time.perf_counter() - t0) / iters * 1000.0

        free_after, _ = cp.cuda.runtime.memGetInfo()
        result = {
            "gpu": gpu,
            "mode": mode,
            "in": f"{in_w}x{in_h}",
            "out": f"{out_w}x{out_h}",
            "ms_per_frame": round(ms, 2),
            "fps_at_budget": round(1000.0 / ms, 1) if ms else None,
            "vram_used_during_run_mb": (free_before - free_after) // (1024 * 1024),
            "vram_free_before_mb": free_before // (1024 * 1024),
        }
        del x, last, sr
        cp.get_default_memory_pool().free_all_blocks()
    return result


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("gpu", nargs="?", type=int, default=0)
    ap.add_argument("--quick", action="store_true")
    args = ap.parse_args()

    if cp.cuda.runtime.getDeviceCount() <= args.gpu:
        print(f"no CUDA device {args.gpu}", file=sys.stderr)
        return 1

    name = cp.cuda.runtime.getDeviceProperties(args.gpu)["name"]
    print(f"# GPU {args.gpu}: {name.decode()}")

    if args.quick:
        cases = [(1, 1280, 720, 2560, 1440), (2, 1280, 720, 2560, 1440)]
    else:
        # (mode, in_w, in_h, out_w, out_h)
        cases = [
            (1, 1280, 720, 2560, 1440),   # LOW 720p->1440p
            (2, 1280, 720, 2560, 1440),   # MED 720p->1440p
            (3, 1280, 720, 2560, 1440),   # HIGH 720p->1440p
            (2, 1920, 1080, 3840, 2160),  # MED 1080p->4K
            (3, 1920, 1080, 3840, 2160),  # HIGH 1080p->4K
            (4, 1920, 1080, 3840, 2160),  # ULTRA 1080p->4K
        ]

    ok = fail = 0
    for mode, iw, ih, ow, oh in cases:
        try:
            r = bench(args.gpu, mode, iw, ih, ow, oh)
            print(r)
            ok += 1
        except Exception as e:  # OOM / unsupported — record and continue
            msg = str(e).splitlines()[0][:200]
            print(f'{{"gpu": {args.gpu}, "mode": {mode}, '
                  f'"in": "{iw}x{ih}", "out": "{ow}x{oh}", '
                  f'"error": "{msg}"}}', file=sys.stderr)
            fail += 1

    print(f"# done: {ok} ok, {fail} failed", file=sys.stderr)
    return 0 if ok else 2


if __name__ == "__main__":
    sys.exit(main())
