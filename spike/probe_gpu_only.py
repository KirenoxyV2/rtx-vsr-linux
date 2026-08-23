#!/usr/bin/env python3
"""
probe_gpu_only.py — isolate the GPU-resident cost of the VSR stage.

The full-pipeline probe (probe_pipeline.py) uses CPU cv2 decode + encode on
purpose, to be conservative. But production route B uses NVDEC (outputs straight
to GPU memory) + NVENC (encodes from GPU memory), so the decode and encode
stages don't touch the CPU and there's no full-frame H2D/D2H for them.

This probe simulates that: frames arrive as CUDA arrays (as if from NVDEC),
stay on the GPU through the format-convert -> VSR -> convert, and we measure
only what the production pipeline actually pays per frame on the AI path.

It is NOT a full video test — it's the headroom proof for the proxy design:
"if the AI stage costs X ms/frame on-GPU, and NVDEC/NVENC add Y ms each,
do we beat real-time?"

Usage:
    source spike/env.sh
    .venv/bin/python spike/probe_gpu_only.py [gpu] [--mode M] [--scale S]
"""
import argparse
import os
import time

import cupy as cp


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("gpu", nargs="?", type=int, default=0)
    ap.add_argument("--mode", type=int, default=2)
    ap.add_argument("--scale", type=float, default=1.5)
    ap.add_argument("--in-w", type=int, default=1280)
    ap.add_argument("--in-h", type=int, default=720)
    ap.add_argument("--iters", type=int, default=120)
    args = ap.parse_args()

    from nvvfx import VideoSuperRes

    dev = cp.cuda.Device(args.gpu)
    with dev:
        name = cp.cuda.runtime.getDeviceProperties(args.gpu)["name"].decode()
        free, total = cp.cuda.runtime.memGetInfo()
        print(f"# GPU {args.gpu}: {name}")
        print(f"# VRAM free {free // (1024*1024)} MB / {total // (1024*1024)} MB")

        ow, oh = int(args.in_w * args.scale), int(args.in_h * args.scale)
        # Simulate NVDEC output: a frame already living in GPU memory (uint8 RGB HWC).
        x_src = (cp.random.rand(args.in_h, args.in_w, 3) * 255).astype(cp.uint8)

        sr = VideoSuperRes(quality=args.mode, device=args.gpu)
        sr.output_width = ow
        sr.output_height = oh
        sr.load()

        # warmup
        for _ in range(3):
            x = cp.ascontiguousarray(cp.transpose(x_src, (2, 0, 1)).astype(cp.float32) / 255.0)
            _ = cp.from_dlpack(sr.run(x).image)
        dev.synchronize()

        t0 = time.perf_counter()
        for _ in range(args.iters):
            # up-convert: HWC uint8 -> CHW float32 [0,1] (stays on GPU)
            x = cp.ascontiguousarray(cp.transpose(x_src, (2, 0, 1)).astype(cp.float32) / 255.0)
            # VSR on GPU
            out = cp.from_dlpack(sr.run(x).image)
            dev.synchronize()
        ms = (time.perf_counter() - t0) / args.iters * 1000.0

        print(f"# in {args.in_w}x{args.in_h} -> out {ow}x{oh}, mode={args.mode}")
        print(f"# GPU-resident AI path (convert + VSR, frames on GPU):")
        print(f"   {ms:.2f} ms/frame  ->  {1000.0/ms:.0f} fps ceiling for the AI stage alone")
        for fps in (30, 60):
            budget = 1000.0 / fps
            print(f"   vs {fps} fps budget ({budget:.2f} ms): "
                  f"{budget/ms:.1f}x headroom  {'OK' if budget > ms else 'TOO SLOW'}")

        # The production pipeline adds NVDEC (~1-3ms) + NVENC (~2-8ms @ these res).
        # Show the combined estimate.
        print(f"\n# Production estimate (NVDEC + this + NVENC):")
        for dec, enc in ((2.0, 4.0), (3.0, 8.0)):
            tot = ms + dec + enc
            for fps in (30, 60):
                b = 1000.0 / fps
                print(f"   NVDEC~{dec:.0f} + AI {ms:.1f} + NVENC~{enc:.0f} = {tot:.1f} ms "
                      f"-> {b/tot:.1f}x vs {fps} fps {'OK' if b > tot else 'TOO SLOW'}")

    return 0


if __name__ == "__main__":
    main()
