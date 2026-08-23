#!/usr/bin/env python3
"""Exercise the resolution policy end-to-end against the running proxy."""
import json, urllib.request, time

BASE = "http://127.0.0.1:8730"
CLIPS = "/home/nilloc/VSR/spike"

def hit(name, src, mode, scale):
    url = f"{BASE}/upscale?src={CLIPS}/{src}&mode={mode}&scale={scale}"
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(url, timeout=300) as r:
            meta = json.loads(r.headers.get("X-VSR-Meta", "{}"))
            body = r.read()
            code = r.status
    except urllib.error.HTTPError as e:
        meta = {"error": e.read().decode()[:120]}; body = b""; code = e.code
    dt = time.perf_counter() - t0
    pt = "PASS-THROUGH" if meta.get("passthrough") else \
         f"VSR {meta.get('mode')} (req {meta.get('mode_requested')}, ds={meta.get('downshifted')})"
    print(f"{name:28s} http={code} {dt:6.2f}s  {meta.get('in','?')} -> {meta.get('out','?')}  "
          f"scale={meta.get('scale')}  {pt:40s} {len(body)/1e6:.2f} MB")

print("=== resolution policy tests (cap=1440p) ===")
hit("720p @1.5 (normal)",   "sample_720p.mp4",            2, 1.5)  # -> 1080p VSR
hit("720p @2.0 (at cap)",   "sample_720p.mp4",            2, 2.0)  # -> 1440p VSR (capped, fits)
hit("720p @3.0 (over cap)", "sample_720p.mp4",            2, 3.0)  # requested 2160p -> capped to 1440p
hit("1080p @1.5 (over cap)","probe_out_2_1920x1080.mp4",  2, 1.5)  # requested 1632p -> capped to 1440p
hit("1440p source (>=cap)", "probe_out_3_2560x1440.mp4",  4, 1.5)  # -> pass-through, no VSR
