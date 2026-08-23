#!/usr/bin/env python3
"""repro_case.py — run ONE VSR case in a fresh process; report VRAM + outcome.

Each invocation is a clean CUDA context (no sticky corruption from prior cases),
so we can pinpoint exactly which mode/resolution the engine rejects and whether
it correlates with available VRAM.

Usage:
    source spike/env.sh
    .venv/bin/python spike/repro_case.py <gpu> <mode> <in_w> <in_h> <scale>
"""
import sys
import time


def main():
    gpu, mode, iw, ih, scale = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), \
        int(sys.argv[4]), float(sys.argv[5])
    ow, oh = (iw * scale) // 16 * 16, (ih * scale) // 16 * 16

    import cupy as cp
    from nvvfx import VideoSuperRes

    dev = cp.cuda.Device(gpu)
    with dev:
        free0, total = cp.cuda.runtime.memGetInfo()
        print(f"# gpu={gpu} mode={mode} in={iw}x{ih} out={ow}x{oh}")
        print(f"# VRAM free BEFORE: {free0//1024//1024} GB ({free0//1024//1024}.{(free0//1024)%1024//102})")

        sr = VideoSuperRes(quality=mode, device=gpu)
        try:
            sr.output_width = ow
            sr.output_height = oh
            sr.load()
            # warmup (the line the proxy died on)
            x = cp.random.rand(3, ih, iw, dtype=cp.float32)
            res = sr.run(x)
            out = cp.array(cp.from_dlpack(res.image))
            dev.synchronize()
            del res, out, x
            free1, _ = cp.cuda.runtime.memGetInfo()
            used = (free0 - free1) // (1024 * 1024)
            print(f"# RESULT: OK   (engine + buffers used ~{used} MB)")
            return 0
        except Exception as e:
            free1, _ = cp.cuda.runtime.memGetInfo()
            print(f"# RESULT: FAIL  free_now={free1//1024//1024} GB")
            print(f"#           {type(e).__name__}: {str(e).splitlines()[0][:160]}")
            return 1


if __name__ == "__main__":
    sys.exit(main())
