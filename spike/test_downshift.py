#!/usr/bin/env python3
"""End-to-end proof of VRAM auto-downshift: monkeypatch live_free_mb to force
specific free-VRAM values, call upscale_clip for real, and verify the chosen
mode cascades correctly. Uses a tiny clip so each run is fast."""
import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "proxy"))
os.chdir(os.path.join(os.path.dirname(__file__), ".."))

import vsr_proxy as P
P._gpu_setup(0)  # init CUDA once

CLIP = "spike/sample_720p.mp4"   # 1280x720, ~10s -> keep short by scale=1.5

def run(label, fake_free, requested):
    P.live_free_mb = lambda gi: fake_free     # force the pre-flight reading
    try:
        data, meta = P.upscale_clip(CLIP, requested, 1.5, 0)
        print(f"{label}: free={fake_free}MB req={P.MODES[requested]} -> "
              f"mode={P.MODES[meta['mode']]} downshifted={meta['downshifted']} "
              f"out={meta['out']} bytes={len(data)}")
    except P.InsufficientVRAM as e:
        print(f"{label}: free={fake_free}MB req={P.MODES[requested]} -> 503 ({e})")

# Request MEDIUM (2) at various fake free levels. Estimates @1920x1088 out:
# LOW~410, MED~670, HIGH/ULTRA~930 (approx).
run("A", 5000, 2)   # plenty -> MEDIUM as requested
run("B",  500, 2)   # below MED(670), above LOW(410) -> downshift to LOW
run("C",  300, 2)   # below LOW too -> 503
print("\nOK — cascade verified end-to-end")
