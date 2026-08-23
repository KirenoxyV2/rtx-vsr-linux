#!/usr/bin/env python3
"""Correctness check: prove the VSR effect is doing real per-mode work.

1) Same input, mode 0 (bicubic) vs mode 3 (HIGH): outputs must differ.
2) Different inputs -> different outputs (no cached/constant output).
3) Report mean pixel diff and max abs diff as evidence.
"""
import sys

import cupy as cp


def main() -> int:
    from nvvfx import VideoSuperRes

    gpu = 0
    dev = cp.cuda.Device(gpu)
    with dev:
        x1 = cp.random.rand(3, 720, 1280, dtype=cp.float32)
        x2 = cp.random.rand(3, 720, 1280, dtype=cp.float32)

        outs = {}
        for mode in (0, 1, 3):
            sr = VideoSuperRes(quality=mode, device=gpu)
            sr.output_width = 2560
            sr.output_height = 1440
            sr.load()
            # clone before teardown: the DLPack capsule aliases effect buffers
            o1 = cp.from_dlpack(sr.run(x1).image).copy()
            o2 = cp.from_dlpack(sr.run(x2).image).copy()
            del sr
            outs[mode] = (o1, o2)

        d_03 = float(cp.abs(outs[0][0] - outs[3][0]).mean())
        m_03 = float(cp.abs(outs[0][0] - outs[3][0]).max())
        d_in = float(cp.abs(outs[1][0] - outs[1][1]).mean())

    print(f"bicubic-vs-HIGH mean diff: {d_03:.6f} (max {m_03:.4f})")
    print(f"different-inputs mean diff (mode 1): {d_in:.6f}")
    ok = d_03 > 1e-5 and d_in > 1e-5
    print("PASS: model is doing real per-mode work" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
