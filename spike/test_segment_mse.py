#!/usr/bin/env python3
"""Check proxy/vsr_segment.py against what MSE needs.

Builds a real DASH stream (separate init segment, 2 s media segments, B-frames,
edit list) from the sample clip and transforms segments from the middle of it,
the way the extension does: init + one bare `moof+mdat` at a time. Verifies

  - every output frame sits on the same PTS as its input frame,
  - the first fragment's tfdt is that absolute time (not rebased to 0),
  - PTS == DTS and each fragment starts on a keyframe (no B-frames),
  - the output init segment is identical from one segment to the next.

Usage:  ~/vsr-venv/bin/python spike/test_segment_mse.py
"""
import io
import os
import subprocess
import sys
import tempfile
from fractions import Fraction

import av
import imageio_ffmpeg

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "proxy"))
import vsr_segment  # noqa: E402


def make_dash(dst):
    subprocess.run([
        imageio_ffmpeg.get_ffmpeg_exe(), "-y", "-loglevel", "error",
        "-i", os.path.join(HERE, "sample_720p.mp4"),
        "-vf", "scale=640:360", "-an",
        "-c:v", "libx264", "-preset", "veryfast", "-bf", "3", "-g", "60",
        "-keyint_min", "60", "-sc_threshold", "0",
        "-f", "dash", "-seg_duration", "2", "-use_template", "1",
        "-use_timeline", "0", os.path.join(dst, "manifest.mpd")], check=True)
    init = open(os.path.join(dst, "init-stream0.m4s"), "rb").read()
    chunks = sorted(f for f in os.listdir(dst) if f.startswith("chunk-stream0"))
    return init, [open(os.path.join(dst, f), "rb").read() for f in chunks]


def packets(data):
    """[(pts, dts, keyframe)] in seconds, as an MSE parser would see them
    (Chromium applies a single-entry edit list just like FFmpeg does)."""
    with av.open(io.BytesIO(data)) as c:
        s = c.streams.video[0]
        size = (s.width, s.height)
        out = [(p.pts * s.time_base, p.dts * s.time_base, p.is_keyframe)
               for p in c.demux(s) if p.pts is not None]
    return out, size


def first_tfdt(data):
    """(baseMediaDecodeTime, timescale) of the first fragment / the track."""
    timescale = tfdt = None
    for kind, a, b in vsr_segment.iter_boxes(data):
        box = data[a:b]
        if kind == b"moov":
            i = box.index(b"mdhd")
            ver = box[i + 4]
            timescale = int.from_bytes(box[i + (24 if ver else 16):i + (28 if ver else 20)], "big")
        if kind == b"moof" and tfdt is None:
            i = box.index(b"tfdt")
            ver = box[i + 4]
            tfdt = int.from_bytes(box[i + 8:i + (16 if ver else 12)], "big")
    return tfdt, timescale


def main():
    ok = True
    with tempfile.TemporaryDirectory() as d:
        init, chunks = make_dash(d)
    print(f"DASH source: init {len(init)} B + {len(chunks)} segments")
    vsr_segment.gpu_setup(0)

    inits = set()
    for idx in (0, 2, len(chunks) - 1):
        src_pk, src_size = packets(init + chunks[idx])
        out, meta = vsr_segment.transform_segment(
            chunks[idx], mode=2, scale=2.0, max_h=1440, gpu=0, init=init)
        assert out is not None, meta
        out_init, out_media = vsr_segment.split_init(out)
        inits.add(out_init)
        out_pk, out_size = packets(out)
        tfdt, ts = first_tfdt(out)

        src_pts = sorted(p[0] for p in src_pk)
        out_pts = [p[0] for p in out_pk]
        kinds = [k.decode() for k, _, _ in vsr_segment.iter_boxes(out)]
        checks = {
            "frame count": len(out_pts) == len(src_pts),
            "pts match input": out_pts == src_pts,
            "pts == dts": all(p[0] == p[1] for p in out_pk),
            "starts on keyframe": out_pk[0][2],
            "tfdt is absolute": Fraction(tfdt, ts) == src_pts[0],
            "init_len": meta["init_len"] == len(out_init) and kinds[:2] == ["ftyp", "moov"],
            "media is moof/mdat": set(kinds[2:]) <= {"moof", "mdat", "mfra"} and "moof" in kinds,
            "2x size": out_size == (src_size[0] * 2, src_size[1] * 2),
        }
        ok &= all(checks.values())
        bad = [k for k, v in checks.items() if not v]
        print(f"segment {idx}: {src_size[0]}x{src_size[1]} -> {out_size[0]}x{out_size[1]}, "
              f"{len(out_pts)} frames, pts {float(src_pts[0]):.3f}..{float(src_pts[-1]):.3f} s, "
              f"tfdt {float(Fraction(tfdt, ts)):.3f} s, "
              f"{meta['ms'] / meta['frames']:.1f} ms/frame  "
              f"{'ok' if not bad else 'FAILED: ' + ', '.join(bad)}")
    same = len(inits) == 1
    ok &= same
    print(f"output init segment identical across segments: {same}")
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    rc = main()
    sys.stdout.flush()
    os._exit(rc)   # nvvfx teardown can double-free; skip interpreter exit
