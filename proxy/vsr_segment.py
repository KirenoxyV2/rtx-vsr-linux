#!/usr/bin/env python3
"""vsr_segment.py — MSE-correct VSR transform for media segments.

The original proxy re-encoded whole clips to a monolithic MP4 — fine for the
demo page, unusable for MSE: browsers append *segments* to typed
SourceBuffers and need (a) an init segment matching the SourceBuffer's
codec, (b) media segments whose timestamps sit where the originals did,
(c) no B-frame negative-DTS surprises at append boundaries.

transform_segment() takes one media segment (fMP4 `moof+mdat`, with the
stream's init segment passed alongside; or anything self-contained that
FFmpeg can open) and returns fragmented H.264/MP4:

  - Timestamps: every output frame carries the PTS the browser's MSE parser
    would have computed for the input frame (FFmpeg and Chromium both apply
    the init segment's single-entry edit list, which cancels the B-frame
    delay), and the fragments' `tfdt` is absolute — so a transformed segment
    lands on the timeline exactly where the original would have.
  - No B-frames, fixed GOP: PTS == DTS, every fragment starts on a keyframe.
  - The output begins with its own init segment (`ftyp+moov`); meta
    ["init_len"] says where it ends. The caller appends it once per
    SourceBuffer (and again whenever it changes), then only the media part.
  - Codec policy: output is always H.264. Appending it to a SourceBuffer
    created for another codec needs `changeType()` first; the extension
    avoids that by steering the page to avc1.

API (used by the proxy's /transform endpoint):
  transform_segment(data, mode, scale, max_h, gpu, init=None) -> (bytes|None, meta)
    None means "passthrough" (nothing to do).
  split_init(data) -> (init_bytes, media_bytes)
"""

import io
import os
import threading
import time
from fractions import Fraction


# ---------------------------------------------------------------------------
# GPU state (mirrors vsr_proxy's model, kept local so this module stands alone)
# ---------------------------------------------------------------------------
GPU = {"ready": False, "err": None}
GPU_LOCK = threading.Lock()

MODES = {1: "LOW", 2: "MEDIUM", 3: "HIGH", 4: "ULTRA"}


def _even(n):
    return max(2, int(round(n / 2.0)) * 2)


def iter_boxes(data):
    """Yield (type, start, end) for each complete top-level ISO-BMFF box."""
    pos, n = 0, len(data)
    while pos + 8 <= n:
        size = int.from_bytes(data[pos:pos + 4], "big")
        kind = bytes(data[pos + 4:pos + 8])
        if size == 1:
            if pos + 16 > n:
                return
            size = int.from_bytes(data[pos + 8:pos + 16], "big")
        elif size == 0:
            size = n - pos
        if size < 8 or pos + size > n:
            return
        yield kind, pos, pos + size
        pos += size


def split_init(data):
    """Split fMP4 bytes into (init segment, media): everything up to the first
    box that belongs to a media segment (`styp`/`sidx`/`moof`) is init."""
    for kind, start, _ in iter_boxes(data):
        if kind in (b"moof", b"styp", b"sidx"):
            return bytes(data[:start]), bytes(data[start:])
    return bytes(data), b""


def gpu_setup(gpu_index=0):
    if GPU["ready"] and GPU.get("gpu_index") == gpu_index:
        return
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
        """float32 CHW [0,1] on the GPU -> uint8 HWC on the host."""
        cp = self.cp
        res = self.sr.run(x)          # keep alive while its image is read
        up = cp.from_dlpack(res.image)
        rgb = cp.ascontiguousarray(
            (cp.clip(up, 0.0, 1.0) * 255.0 + 0.5).astype(cp.uint8).transpose(1, 2, 0))
        self.dev.synchronize()
        del up, res
        return rgb.get()


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
                      max_h: int = 1440, gpu: int = 0, strict: bool = False,
                      init: bytes = None):
    """Upscale one media segment's video track; return (new_bytes, meta).

    `init` is the stream's init segment when `data` is a bare `moof+mdat`.

    passthrough conditions (new_bytes is None):
      - no video track / audio-only / not decodable
      - source height >= max_h
    """
    import av

    meta = {"in": None, "out": None, "mode": mode, "scale": scale,
            "frames": 0, "codec": None}
    src = io.BytesIO((init or b"") + data)
    try:
        inp = av.open(src)
    except av.error.FFmpegError:
        return None, dict(meta, note="not decodable — passthrough")

    try:
        if not inp.streams.video:
            return None, dict(meta, note="audio-only — passthrough")
        vs = inp.streams.video[0]
        src_w, src_h = vs.width, vs.height
        meta["in"] = f"{src_w}x{src_h}"
        meta["codec"] = vs.codec_context.name
        if src_h >= max_h or scale <= 1.0:
            return None, dict(meta, note="source >= cap — passthrough")

        factor = min(scale, max_h / src_h)
        out_w, out_h = _even(src_w * factor), _even(src_h * factor)
        meta["out"] = f"{out_w}x{out_h}"
        with GPU_LOCK:
            gpu_setup(gpu)
            out = _transcode(inp, vs, mode, out_w, out_h, gpu, meta)
    finally:
        inp.close()
    if not meta["frames"]:
        return None, dict(meta, note="no frames (init segment only?) — passthrough")
    meta["init_len"] = len(split_init(out)[0])
    return out, meta


def _transcode(inp, vs, mode, out_w, out_h, gpu, meta):
    import av

    eng = _get_engine(mode, out_w, out_h, gpu, vs.height, vs.width)
    cp = GPU["cupy"]

    # Fragmented MP4, one fragment per GOP. By default the muxer rebases a
    # segment to start at 0; frag_discont + avoid_negative_ts=disabled make it
    # write the real decode time into tfdt.
    out_buf = io.BytesIO()
    outp = av.open(out_buf, "w", format="mp4", options={
        "movflags": "+frag_keyframe+empty_moov+default_base_moof+frag_discont",
        "avoid_negative_ts": "disabled",
    })
    fps = vs.average_rate or vs.guessed_rate or Fraction(30, 1)
    gop = max(1, int(round(float(fps))))
    encoder = os.environ.get("VSR_ENCODER", "libx264")
    ostr = outp.add_stream(encoder, rate=fps)
    ostr.width, ostr.height = out_w, out_h
    ostr.pix_fmt = "yuv420p"
    ostr.time_base = vs.time_base
    ostr.codec_context.time_base = vs.time_base
    if encoder == "libx264":
        ostr.options = {
            "preset": "veryfast",
            "tune": "zerolatency",
            "x264-params": f"keyint={gop}:min-keyint={gop}:scenecut=0:bframes=0",
        }
    else:  # nvenc: no B-frames, for MSE-safety
        ostr.options = {"preset": "p1", "tune": "ll", "bf": "0", "g": str(gop)}

    n = 0
    t0 = time.perf_counter()
    for frame in inp.decode(vs):
        arr = frame.to_ndarray(format="rgb24")
        x = cp.asarray(arr).transpose(2, 0, 1).astype(cp.float32) / 255.0
        rgb = eng.run(cp.ascontiguousarray(x))
        of = av.VideoFrame.from_ndarray(rgb, format="rgb24")
        of.pts = frame.pts
        of.time_base = vs.time_base
        for pkt in ostr.encode(of):
            outp.mux(pkt)
        n += 1
    for pkt in ostr.encode(None):
        outp.mux(pkt)
    outp.close()

    meta["frames"] = n
    meta["ms"] = round((time.perf_counter() - t0) * 1000, 1)
    return out_buf.getvalue()
