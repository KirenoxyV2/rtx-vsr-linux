#!/usr/bin/env python3
"""
probe_pipeline.py — M1.5 end-to-end latency probe for the route-B proxy.

Answers the one question that could change the architecture: does a REAL
decode -> VSR -> encode pipeline run faster than real-time on this box, and
where does the time actually go?

It is deliberately built with *slower* I/O than production (CPU cv2 decode +
CPU mp4v encode) while VSR runs on GPU. So if THIS stays under real-time, the
production NVDEC+NVENC pipeline has even more headroom. Conservative by design.

Pipeline per frame:
  1. decode   : cv2 VideoCapture -> BGR uint8 HxWx3 (CPU)          [measured]
  2. up-conv  : cupy H2D + BGR->RGB + HWC->CHW + /255 float32      [measured]
  3. VSR      : nvvfx VideoSuperRes.run() on GPU                   [measured]
  4. down-conv: output -> uint8 HWC BGR + D2H                      [measured]
  5. encode   : cv2 VideoWriter mp4v (CPU)                         [measured]

Outputs:
  - per-stage ms/frame breakdown + total vs real-time budget
  - <out>.mp4            the upscaled video
  - compare_<n>.png      A/B montage: bicubic upscale vs VSR (same crop/scale)

Usage:
    source spike/env.sh
    .venv/bin/python spike/probe_pipeline.py [gpu] [--frames N] [--mode M]
"""
import argparse
import os
import sys
import time

import cv2
import numpy as np


def fmt_mb(nbytes):
    return f"{nbytes / (1024 * 1024):.0f} MB"


def normalize_output(img):
    """Turn the VSR output (unknown CHW/HWC, float or uint8) into BGR uint8 HxWx3 numpy."""
    if img.dtype in (np.float32, np.float16):
        img = (img * 255.0).round()
    img = img.astype(np.uint8)
    # channels-first?
    if img.ndim == 3 and img.shape[0] in (1, 3, 4) and img.shape[0] < img.shape[1]:
        img = np.ascontiguousarray(img.transpose(1, 2, 0))
    if img.ndim == 3 and img.shape[2] == 3:
        img = img[:, :, ::-1]  # RGB -> BGR
    return img


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("gpu", nargs="?", type=int, default=0)
    ap.add_argument("--frames", type=int, default=0, help="0 = all")
    ap.add_argument("--mode", type=int, default=3, help="VSR quality 1-4 (default 3=HIGH)")
    ap.add_argument("--scale", type=float, default=2.0)
    ap.add_argument("--clip", default=os.path.join(os.path.dirname(__file__), "sample_720p.mp4"))
    args = ap.parse_args()

    import cupy as cp
    from nvvfx import VideoSuperRes

    dev = cp.cuda.Device(args.gpu)
    with dev:
        name = cp.cuda.runtime.getDeviceProperties(args.gpu)["name"].decode()
        free, total = cp.cuda.runtime.memGetInfo()
        print(f"# GPU {args.gpu}: {name}")
        print(f"# VRAM free {fmt_mb(free)} / {fmt_mb(total)}")

        # --- decode setup ---
        cap = cv2.VideoCapture(args.clip)
        if not cap.isOpened():
            print(f"cannot open {args.clip}", file=sys.stderr)
            return 1
        src_fps = cap.get(cv2.CAP_PROP_FPS) or 0.0
        src_w = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
        src_h = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
        n_total = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
        out_w, out_h = int(src_w * args.scale), int(src_h * args.scale)
        n_frames = args.frames if args.frames > 0 else min(n_total, 300)
        print(f"# source: {src_w}x{src_h} @ {src_fps:.2f} fps, {n_total} frames")
        print(f"# VSR mode={args.mode}, out={out_w}x{out_h}, processing {n_frames} frames")

        # --- VSR effect (warmup once) ---
        t0 = time.perf_counter()
        sr = VideoSuperRes(quality=args.mode, device=args.gpu)
        sr.output_width = out_w
        sr.output_height = out_h
        sr.load()
        init_ms = (time.perf_counter() - t0) * 1000.0

        # dummy warmup frame to prime engine + allocations
        x_dummy = cp.random.rand(3, src_h, src_w, dtype=cp.float32)
        _ = cp.from_dlpack(sr.run(x_dummy).image)
        dev.synchronize()
        del x_dummy

        # --- encode setup (CPU mp4v; conservative vs production NVENC) ---
        fourcc = cv2.VideoWriter_fourcc(*"mp4v")
        out_path = os.path.join(os.path.dirname(__file__), f"probe_out_{args.mode}_{out_w}x{out_h}.mp4")
        writer = cv2.VideoWriter(out_path, fourcc, src_fps, (out_w, out_h))
        if not writer.isOpened():
            print("VideoWriter failed to open", file=sys.stderr)
            return 1

        # --- stage accumulators (ms) ---
        st = {"decode": 0.0, "upconv": 0.0, "vsr": 0.0, "downconv": 0.0, "encode": 0.0}
        first_out = None
        first_src = None

        for i in range(n_frames):
            # 1. decode (CPU)
            t = time.perf_counter()
            ok, frame_bgr = cap.read()
            if not ok:
                break
            st["decode"] += (time.perf_counter() - t) * 1000.0

            # 2. up-convert to VSR input format on GPU
            t = time.perf_counter()
            x = cp.asarray(frame_bgr[:, :, ::-1])          # BGR->RGB, H2D
            x = cp.ascontiguousarray(cp.transpose(x, (2, 0, 1)).astype(cp.float32) / 255.0)
            dev.synchronize()
            st["upconv"] += (time.perf_counter() - t) * 1000.0

            # 3. VSR on GPU
            t = time.perf_counter()
            out_img = cp.from_dlpack(sr.run(x).image)
            dev.synchronize()
            st["vsr"] += (time.perf_counter() - t) * 1000.0

            # 4. down-convert to BGR uint8 + D2H
            t = time.perf_counter()
            out_np = out_img.get() if isinstance(out_img, cp.ndarray) else np.asarray(out_img)
            frame_np = normalize_output(out_np)
            dev.synchronize()
            st["downconv"] += (time.perf_counter() - t) * 1000.0

            # keep first pair for the A/B still
            if i == 0:
                first_src = frame_bgr.copy()
                first_out = frame_np.copy()

            # 5. encode (CPU)
            t = time.perf_counter()
            writer.write(frame_np)
            st["encode"] += (time.perf_counter() - t) * 1000.0

        cap.release()
        writer.release()

        n = i + 1
        # --- report ---
        print("\n# ---- per-stage ms/frame ----")
        total = 0.0
        for k in ("decode", "upconv", "vsr", "downconv", "encode"):
            v = st[k] / n
            total += v
            bar = "#" * int(v)
            print(f"  {k:<9} {v:8.2f} ms   {bar}")
        print(f"  {'TOTAL':<9} {total:8.2f} ms")

        budget_60 = 1000.0 / 60.0
        budget_src = 1000.0 / src_fps if src_fps else 0.0
        print("\n# ---- real-time verdict ----")
        print(f"  source fps           : {src_fps:.2f}  (budget {budget_src:.2f} ms/frame)")
        print(f"  pipeline total       : {total:.2f} ms/frame")
        print(f"  effective throughput : {1000.0 / total:.1f} frames/s")
        if src_fps:
            ratio = budget_src / total
            print(f"  headroom vs source fps : {ratio:.2f}x  "
                  f"({'REAL-TIME with margin' if ratio > 1 else 'CANNOT keep up'})")
        print(f"  VSR init (one-time)  : {init_ms:.0f} ms")

        # --- A/B comparison still: bicubic upscale vs VSR, same crop/scale ---
        if first_src is not None and first_out is not None:
            ch, cw = first_src.shape[:2]
            # pick a crop that fits the source and, scaled by args.scale, fits the output
            scale = args.scale
            crop_h = min(540, ch)
            crop_w = min(int(crop_h * cw / ch), cw)
            sx = (ch - crop_h) // 2
            sy = (cw - crop_w) // 2
            src_crop = first_src[sx:sx + crop_h, sy:sy + crop_w]
            # bicubic upscale of that crop to the SAME size as the VSR region
            disp_w, disp_h = int(crop_w * scale), int(crop_h * scale)
            bicubic = cv2.resize(src_crop, (disp_w, disp_h), interpolation=cv2.INTER_CUBIC)
            # matching center crop from VSR output at scaled coords
            ox = sx * scale
            oy = sy * scale
            vsr_crop = first_out[int(oy):int(oy + disp_h), int(ox):int(ox + disp_w)]
            # normalize heights in case of rounding
            if vsr_crop.shape[0] != bicubic.shape[0]:
                vsr_crop = cv2.resize(vsr_crop, (bicubic.shape[1], bicubic.shape[0]))
            label_b = np.full((40, bicubic.shape[1], 3), (60, 60, 60), np.uint8)
            cv2.putText(label_b, "bicubic (no VSR)", (10, 27),
                        cv2.FONT_HERSHEY_SIMPLEX, 0.9, (255, 255, 255), 2)
            label_v = np.full((40, vsr_crop.shape[1], 3), (60, 60, 60), np.uint8)
            cv2.putText(label_v, f"RTX VSR mode {args.mode}", (10, 27),
                        cv2.FONT_HERSHEY_SIMPLEX, 0.9, (80, 220, 80), 2)
            top = np.vstack([label_b, bicubic])
            bot = np.vstack([label_v, vsr_crop])
            if top.shape[0] != bot.shape[0]:
                pad = np.full((abs(top.shape[0] - bot.shape[0]), bot.shape[1], 3), 0, np.uint8)
                if top.shape[0] < bot.shape[0]:
                    top = np.vstack([top, pad])
                else:
                    bot = np.vstack([bot, pad])
            montage = np.hstack([top, bot])
            still = os.path.join(os.path.dirname(__file__), f"compare_mode{args.mode}.png")
            cv2.imwrite(still, montage)
            print(f"\n# wrote: {out_path}")
            print(f"# wrote: {still}  (left=bicubic, right=VSR; same crop)")
        else:
            print(f"\n# wrote: {out_path}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
