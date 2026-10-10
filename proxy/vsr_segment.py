#!/usr/bin/env python3
"""vsr_segment.py — MSE-correct VSR transform for media segments.

The original proxy re-encoded whole clips to a monolithic MP4 — fine for the
demo page, unusable for MSE: browsers append *segments* to typed
SourceBuffers and need (a) an init segment matching the SourceBuffer's
codec, (b) media segments with continuous timestamps, (c) no B-frame
negative-DTS surprises at append boundaries.

This module transforms one media segment (fMP4/CMAF, WebM, or standalone
MP4) preserving the container type:

  - Input fMP4 with init segment (moov+moof): transform media segments;
    generate a matching H.264 init segment on first use (cached per
    stream signature) and rewrite it so the codec/track matches what we
    append next.
  - Timestamps: output PTS mirrors input PTS 1:1 (no re-time), muxed with
    empty-base-is-default -1moof style fragmentation so each appended
    segment is self-contained.
  - Codec policy: input H.264 -> output H.264 (byte-compatible for MSE in
    most browsers). VP9/AV1 inputs: output H.264 — callers must switch to a
    new video SourceBuffer (the extension handles this by only
    intercepting H.264 sources, or by creating a parallel buffer).

API (used by the proxy's /transform endpoint):
  transform_segment(data: bytes, mode, scale, max_h, gpu) -> (bytes|None, meta)
    None means "passthrough" (nothing to do).
  get_init(src_init_bytes) -> bytes            # upscaled-stream init segment
"""

import io
import os
import threading
import time
from fractions import Fraction

import numpy as np

# ---------------------------------------------------------------------------
# GPU state (mirrors vsr_proxy's model, kept local so this module stands alone)
# ---------------------------------------------------------------------------
GPU = {"ready": False, "err": None}
GPU_LOCK = threading.Lock()

MODES = {1: "LOW", 2: "MEDIUM", 3: "HIGH", 4: "ULTRA"}


def _align16(n):
    return max(16, int(round(n / 16.0)) * 16)


def gpu_setup(gpu_index=0):
    import cupy as cp
    from nvvfx import VideoSuperRes

    dev = cp.cuda.Device(gpu_index)
    with dev:
        name = cp.cuda.runtime.getDeviceProperties(gpu_index)["name"].decode()
        free, total = cp.cuda.runtime.memGetInfo()
    GPU.update(ready=True, err=None, cupy=cp, VideoSuperRes=VideoSuperRes,
               gpu_index=gpu_index, name=name,
               free_mb=free // (1024 * 1024), total_mb=total // (1024 * 1024))


class _Engine:
    """One persistent VSR engine per (mode, out_w, out_h) — reused across
    segments of a stream (the model is stateful per resolution pair)."""

    def __init__(self, mode, out_w, out_h, gpu):
        from nvvfx import VideoSuperRes
        import cupy as cp

        self.cp = cp
        self.dev = cp.cuda.Device(gpu)
        self.sr = VideoSuperRes(quality=mode, device=gpu)
        self.sr.output_width = out_w
        self.sr.output_height = out_h
        self.sr.load()
        self.out_w, self.out_h = out_w, out_h
        self.warmed = False

    def warm(self, src_h, src_w):
        if self.warmed:
            return
        x = self.cp.random.rand(3, src_h, src_w, dtype=self.cp.float32)
        res = self.sr.run(x)
        img = self.cp.array(self.cp.from_dlpack(res.image))  # deep copy
        self.dev.synchronize()
        del res, img, x
        self.warmed = True

    def run(self, x):
        res = self.sr.run(x)
        out = self.cp.array(self.cp.from_dlpack(res.image))  # deep copy
        del res
        self.dev.synchronize()
        return out


_ENGINES = {}


def _get_engine(mode, out_w, out_h, gpu, src_h, src_w):
    key = (mode, out_w, out_h, gpu)
    eng = _ENGINES.get(key)
    if eng is None:
        eng = _Engine(mode, out_w, out_h, gpu)
        _ENGINES[key] = eng
    eng.warm(src_h, src_w)
    return eng


# ---------------------------------------------------------------------------
# Segment transform (PyAV in/out)
# ---------------------------------------------------------------------------

def transform_segment(data: bytes, mode: int = 2, scale: float = 2.0,
                      max_h: int = 1440, gpu: int = 0, strict: bool = False):
    """Upscale one media segment's video track; return (new_bytes, meta).

    passthrough conditions (new_bytes is None):
      - no video track / audio-only
      - source height >= max_h
    """
    import av
    import av.video.frame  # noqa: F401

    src = io.BytesIO(data)
    meta = {"in": None, "out": None, "mode": mode, "scale": scale,
            "frames": 0, "codec": None}

    try:
        inp = av.open(src)
    except av.error.InvalidDataError:
        return None, dict(meta, note="not decodable (init segment?) — passthrough")

    if not inp.streams.video:
        inp.close()
        return None, dict(meta, note="audio-only — passthrough")

    vs = inp.streams.video[0]
    src_w, src_h = vs.width, vs.height
    meta["in"] = f"{src_w}x{src_h}"
    meta["codec"] = vs.codec_context.name

    if src_h >= max_h or scale <= 1.0:
        inp.close()
        return None, dict(meta, note="source >= cap — passthrough")

    out_w = _align16(src_w * scale)
    out_h = _align16(src_h * scale)
    if out_h > max_h:
        out_w = _align16(src_w * (max_h / src_h))
        out_h = _align16(max_h)
    meta["out"] = f"{out_w}x{out_h}"

    eng = _get_engine(mode, out_w, out_h, gpu, src_h, src_w)
    cp = GPU["cupy"]

    # ---- demux+decode -> VSR -> encode+remux, copying timestamps
    # Fragmented MP4 (CMAF-style): each transformed segment appends cleanly to
    # a video/mp4 SourceBuffer. ftyp+moov (init) precede the first moof; the
    # extension strips them after the first append (see split_init helper).
    out_buf = io.BytesIO()
    outp = av.open(out_buf, "w", format="mp4", options={
        "movflags": "+frag_keyframe+empty_moov+default_base_moof",
    })
    src_fps = float(vs.average_rate or 30.0) or 30.0
    encoder = os.environ.get("VSR_ENCODER", "libx264")
    ostr = outp.add_stream(encoder, rate=Fraction(int(round(src_fps)), 1))
    ostr.width, ostr.height = out_w, out_h
    ostr.pix_fmt = "yuv420p"
    ostr.time_base = vs.time_base or Fraction(1, 1000)
    if encoder == "libx264":
        ostr.options = {
            "preset": "veryfast",
            "tune": "zerolatency",
            "x264-params": "keyint=30:min-keyint=30:scenecut=0:bframes=0",
        }
    else:  # nvenc: no B-frames by default for MSE-safety
        ostr.options = {
            "preset": "p1",        # fastest
            "tune": "ll",          # low latency
            "bf": "0",
            "g": "30",
        }

    n = 0
    t0 = time.perf_counter()
    for packet in inp.demux(vs):
        for frame in packet.decode():
            # RGB float CHW for the model
            arr = frame.to_ndarray(format="rgb24").astype(np.float32) / 255.0
            x = cp.asarray(arr.transpose(2, 0, 1))
            out = eng.run(x)
            h = out.get()
            rgb = (np.clip(h.transpose(1, 2, 0), 0, 1) * 255).astype(np.uint8)
            del out, x

            of = av.VideoFrame.from_ndarray(rgb, format="rgb24")
            of.pts = frame.pts
            of.time_base = frame.time_base
            for pkt in ostr.encode(of):
                outp.mux(pkt)
            n += 1
    for pkt in ostr.encode(None):
        outp.mux(pkt)
    outp.close()
    inp.close()

    meta["frames"] = n
    meta["ms"] = round((time.perf_counter() - t0) * 1000, 1)
    return out_buf.getvalue(), meta
