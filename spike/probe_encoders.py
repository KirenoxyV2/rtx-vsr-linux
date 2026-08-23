#!/usr/bin/env python3
"""probe_encoders.py — discover which video encoders are actually usable here.

Browser playback needs H.264 or HEVC. We want GPU (NVENC) for headroom but CPU
libx264 is an acceptable fallback to prove the loop. This probe tries each
fourcc through cv2's VideoWriter and reports which ones actually open + write.
Also reports whether a system/static ffmpeg with nvenc exists.
"""
import os
import shutil
import subprocess
import sys

import cv2
import numpy as np

TMP = "/tmp/enc_probe"
os.makedirs(TMP, exist_ok=True)

# (fourcc string, label, notes)
CANDIDATES = [
    ("h264_nvenc", "NVENC H.264 (GPU)", "ideal"),
    ("hevc_nvenc", "NVENC HEVC (GPU)", "ideal 10-bit"),
    ("avc1",       "libx264 H.264 (CPU)", "browser-safe fallback"),
    ("H264",       "H264 fourcc", ""),
    ("x264",       "x264", ""),
    ("mp4v",       "MPEG-4 Part 2 (CPU)", "NOT browser-safe, baseline only"),
]


def try_fourcc(fourcc_str):
    path = os.path.join(TMP, f"t_{fourcc_str}.mp4")
    fourcc = cv2.VideoWriter_fourcc(*fourcc_str)
    w = cv2.VideoWriter(path, fourcc, 30.0, (1920, 1080))
    ok = w.isOpened()
    wrote = False
    if ok:
        frame = np.random.randint(0, 255, (1080, 1920, 3), dtype=np.uint8)
        for _ in range(3):
            w.write(frame)
        w.release()
        wrote = os.path.getsize(path) > 1000 if os.path.exists(path) else False
    else:
        w.release()
    return ok, wrote


def main():
    print(f"cv2 {cv2.__version__}")
    print("=" * 60)
    results = {}
    for fc, label, note in CANDIDATES:
        try:
            opened, wrote = try_fourcc(fc)
        except Exception as e:
            opened, wrote = False, False
            note = f"exc: {str(e)[:60]}"
        status = "OK " if (opened and wrote) else ("open-only" if opened else "no ")
        results[fc] = (opened and wrote)
        print(f"  [{status}] {fc:<12} {label:<24} {note}")

    # system / static ffmpeg?
    print("=" * 60)
    for ff in ("ffmpeg", "/usr/bin/ffmpeg", "imageio_ffmpeg"):
        p = shutil.which(ff) if not os.path.isabs(ff) else (ff if os.path.exists(ff) else None)
        if p:
            try:
                out = subprocess.run([p, "-hide_banner", "-encoders"],
                                     capture_output=True, text=True, timeout=20).stdout
                has_nvenc = "nvenc" in out
                has_x264 = "libx264" in out
                print(f"  ffmpeg at {p}: nvenc={has_nvenc} libx264={has_x264}")
            except Exception as e:
                print(f"  ffmpeg at {p}: probe failed {e}")
        else:
            print(f"  {ff}: not found")

    # imageio-ffmpeg bundled binary (CPU) — common pip fallback
    try:
        import imageio_ffmpeg
        ex = imageio_ffmpeg.get_ffmpeg_exe()
        out = subprocess.run([ex, "-hide_banner", "-encoders"],
                             capture_output=True, text=True, timeout=20).stdout
        print(f"  imageio-ffmpeg exe {os.path.basename(ex)}: "
              f"nvenc={'nvenc' in out} libx264={'libx264' in out}")
    except Exception as e:
        print(f"  imageio-ffmpeg: not available ({str(e)[:50]})")

    # gcc for a possible C extension (NVENC/nvcuvid) later
    print("=" * 60)
    cc = shutil.which("gcc") or shutil.which("cc")
    print(f"  C compiler: {cc}")


if __name__ == "__main__":
    main()
