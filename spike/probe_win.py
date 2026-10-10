import json
import sys

# Windows-side twin of probe_turing.py: does the SAME wheel's VSR model load on
# THIS GPU (RTX 2060 SUPER, Turing sm_75) under the Windows driver?
# One case per invocation (a failed load can abort the process at exit).


def try_case(idx: int) -> dict:
    import cupy as cp

    dev = cp.cuda.Device(0)
    with dev:
        name = cp.cuda.runtime.getDeviceProperties(0)["name"]
        if isinstance(name, bytes):
            name = name.decode()
        free, _total = cp.cuda.runtime.memGetInfo()

    out = {"case": idx, "gpu": name, "free_mb": free // 2**20}

    if idx == 0:  # BICUBIC (non-AI)
        from nvvfx import QualityLevel, VideoSuperRes

        sr = VideoSuperRes(quality=QualityLevel.BICUBIC, device=0)
        sr.output_width, sr.output_height = 1280, 720
        x = cp.random.rand(3, 360, 640, dtype=cp.float32)
        sr.load()
        res = sr.run(x)
        img = cp.array(cp.from_dlpack(res.image))
        dev.synchronize()
        out.update(effect="VSR/BICUBIC", out_shape=str(img.shape))
        return out

    if idx in (1, 2, 3, 4):  # AI upscale LOW..ULTRA
        from nvvfx import VideoSuperRes

        sr = VideoSuperRes(quality=idx, device=0)
        sr.output_width, sr.output_height = 1280, 720
        x = cp.random.rand(3, 360, 640, dtype=cp.float32)
        sr.load()
        res = sr.run(x)
        img = cp.array(cp.from_dlpack(res.image))
        dev.synchronize()
        out.update(effect=f"VSR/AI-mode{idx}", out_shape=str(img.shape))
        return out

    if idx == 5:  # TrueHDR
        from nvvfx import TrueHDR

        t = TrueHDR(device=0)
        x = cp.random.rand(3, 360, 640, dtype=cp.float32)
        t.load()
        res = t.run(x)
        img = cp.array(cp.from_dlpack(res.image))
        dev.synchronize()
        out.update(effect="TrueHDR", out_shape=str(img.shape))
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
    print("RESULT " + json.dumps(r))
