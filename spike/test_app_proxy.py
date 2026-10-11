#!/usr/bin/env python3
"""Check the app's built-in proxy (app/, `rtx-vsr --headless`) against what MSE needs.

Builds a real DASH stream (separate init segment, 2 s media segments, B-frames,
edit list) from the sample clip, starts the app without a window on a spare
port and sends it segments the way the extension does: init + one bare
`moof+mdat` per request. Verifies

  - every output frame sits on the same PTS as its input frame,
  - the first fragment's tfdt is that absolute time (not rebased to 0),
  - PTS == DTS and each fragment starts on a keyframe (no B-frames),
  - the output init segment is identical from one segment to the next,
  - the picture is the same picture: colours match the source,
  - a page on a smaller display than the chosen resolution gets that
    display's resolution (checked for every display of this machine),
  - asked for in parts, a fragment comes back as the same frames, the first
    of them long before the last; what is behind the playhead changes
    nothing but the speed; and a fragment that is called off is dropped,
  - audio, and video that already fills the target, come back as 204,
  - a web page (as opposed to an extension) is refused.

Usage:  ~/vsr-venv/bin/python spike/test_app_proxy.py [path/to/rtx-vsr]
"""
import io
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from fractions import Fraction

import av
import imageio_ffmpeg
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
PORT = 8742
BASE = f"http://127.0.0.1:{PORT}"


def find_app():
    if len(sys.argv) > 1:
        return sys.argv[1]
    for build in ("release", "debug"):
        p = os.path.join(REPO, "app", "target", build, "rtx-vsr")
        if os.path.exists(p):
            return p
    sys.exit("build the app first: (cd app && cargo build --release)")


def make_dash(dst, size="640:360", audio=False):
    cmd = [imageio_ffmpeg.get_ffmpeg_exe(), "-y", "-loglevel", "error",
           "-i", os.path.join(HERE, "sample_720p.mp4")]
    if audio:
        cmd += ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                "-map", "0:v:0", "-map", "1:a:0", "-shortest", "-c:a", "aac"]
    else:
        cmd += ["-an"]
    cmd += ["-vf", f"scale={size}", "-c:v", "libx264", "-preset", "veryfast",
            "-bf", "3", "-g", "60", "-keyint_min", "60", "-sc_threshold", "0",
            "-f", "dash", "-seg_duration", "2", "-use_template", "1",
            "-use_timeline", "0", os.path.join(dst, "manifest.mpd")]
    subprocess.run(cmd, check=True)

    def stream(i):
        init = open(os.path.join(dst, f"init-stream{i}.m4s"), "rb").read()
        names = sorted(f for f in os.listdir(dst) if f.startswith(f"chunk-stream{i}"))
        return init, [open(os.path.join(dst, f), "rb").read() for f in names]
    return stream


def iter_boxes(data):
    pos, n = 0, len(data)
    while pos + 8 <= n:
        size = int.from_bytes(data[pos:pos + 4], "big")
        if size < 8 or pos + size > n:
            return
        yield bytes(data[pos + 4:pos + 8]), pos, pos + size
        pos += size


def packets(data):
    """[(pts, dts, keyframe)] in seconds, as an MSE parser would see them
    (Chromium applies a single-entry edit list just like FFmpeg does)."""
    with av.open(io.BytesIO(data)) as c:
        s = c.streams.video[0]
        size = (s.width, s.height)
        out = [(p.pts * s.time_base, p.dts * s.time_base, p.is_keyframe)
               for p in c.demux(s) if p.pts is not None]
    return out, size


def picture(data, size, count=6, block=8):
    """A few decoded frames scaled to `size`, as float RGB averaged over
    blocks: the picture without the detail that upscaling is meant to change."""
    out = []
    with av.open(io.BytesIO(data)) as c:
        for i, f in enumerate(c.decode(video=0)):
            if i % 10 == 0:
                out.append(f.reformat(width=size[0], height=size[1], format="rgb24")
                           .to_ndarray().astype(np.float32))
            if len(out) == count:
                break
    x = np.stack(out)
    n, h, w, ch = x.shape
    x = x[:, :h - h % block, :w - w % block]
    return x.reshape(n, h // block, block, w // block, block, ch).mean(axis=(2, 4))


def first_tfdt(data):
    timescale = tfdt = None
    for kind, a, b in iter_boxes(data):
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


def call(path, body=b"", origin="chrome-extension://test"):
    """-> (status, bytes, meta, the name to ask for the next part by)"""
    req = urllib.request.Request(BASE + path, data=body, method="POST")
    if origin:
        req.add_header("Origin", origin)
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            meta = json.loads(r.headers.get("X-VSR-Meta") or "null")
            return r.status, r.read(), meta, r.headers.get("X-VSR-Next")
    except urllib.error.HTTPError as e:
        return e.code, e.read(), None, None


def post(body, query="", origin="chrome-extension://test"):
    """-> (status, bytes, meta)"""
    return call("/transform" + query, body, origin)[:3]


def post_in_parts(body, query=""):
    """The way the extension asks. -> (status, [part, ...], meta of the last,
    seconds until the first part, seconds until the last)"""
    t0 = time.time()
    status, part, meta, more = call("/transform?parts=1" + query, body)
    first, parts = time.time() - t0, [part]
    while status == 200 and more:
        status, part, meta, more = call(f"/transform/next?job={more}")
        parts.append(part)
    return status, parts, meta, first, time.time() - t0


def get(path):
    with urllib.request.urlopen(BASE + path, timeout=10) as r:
        return json.loads(r.read())


def plan(src, box):
    """The app's rule: fit `src` into `box`, at most 4x, even sizes."""
    f = min(box[0] / src[0], box[1] / src[1], 4.0)
    return tuple(max(2, int(v * f / 2 + 0.5) * 2) for v in src)


def start_app(settings):
    """-> (process, its config directory)"""
    config = tempfile.mkdtemp(prefix="vsr-app-config-")
    os.makedirs(os.path.join(config, "rtx-vsr"))
    with open(os.path.join(config, "rtx-vsr", "config.json"), "w") as f:
        json.dump(settings, f)
    proc = subprocess.Popen([find_app(), "--headless", "--port", str(PORT)],
                            env=dict(os.environ, XDG_CONFIG_HOME=config))
    return proc, config


def stop_app(proc, config):
    proc.terminate()
    proc.wait(timeout=10)
    subprocess.run(["rm", "-rf", config])


def wait_up(proc):
    for _ in range(100):
        if proc.poll() is not None:
            sys.exit(f"the app exited with {proc.returncode}")
        try:
            return get("/health")
        except OSError:
            time.sleep(0.1)
    sys.exit("the app did not start listening")


def main():
    ok = True
    with tempfile.TemporaryDirectory() as d:
        init, chunks = make_dash(d)(0)
    with tempfile.TemporaryDirectory() as d:
        stream = make_dash(d, audio=True)
        a_init, a_chunks = stream(1)
    with tempfile.TemporaryDirectory() as d:
        hd_init, hd_chunks = make_dash(d, size="1280:720")(0)
    print(f"DASH source: init {len(init)} B + {len(chunks)} segments")

    def expect(label, got, want):
        nonlocal ok
        good = got == want
        ok &= good
        print(f"{label}: {got} {'ok' if good else f'FAILED (wanted {want})'}")

    app, config = start_app({"target": [1280, 720], "quality": 2})
    try:
        health = wait_up(app)
        print(f"app {health['version']}: gpu={health['gpu']!r} ok={health['ok']} err={health['err']}")
        cfg = get("/config")
        displays = cfg["target"]

        inits = set()
        for idx in (0, 2, len(chunks) - 1):
            src = init + chunks[idx]
            src_pk, src_size = packets(src)
            t0 = time.time()
            status, out, meta = post(src)
            dt = time.time() - t0
            if status != 200:
                print(f"segment {idx}: HTTP {status} {out[:200]!r} {meta}")
                ok = False
                continue
            init_len = meta["init_len"]
            inits.add(out[:init_len])
            out_pk, out_size = packets(out)
            tfdt, ts = first_tfdt(out)
            src_pts = sorted(p[0] for p in src_pk)
            out_pts = [p[0] for p in out_pk]
            kinds = [k.decode() for k, _, _ in iter_boxes(out)]
            diff = np.abs(picture(src, src_size) - picture(out, src_size))
            checks = {
                "frame count": len(out_pts) == len(src_pts),
                "pts match input": out_pts == src_pts,
                "pts == dts": all(p[0] == p[1] for p in out_pk),
                "starts on keyframe": out_pk[0][2],
                "tfdt is absolute": Fraction(tfdt, ts) == src_pts[0],
                "init_len": kinds[:2] == ["ftyp", "moov"] and sum(
                    b - a for k, a, b in iter_boxes(out) if k in (b"ftyp", b"moov")) == init_len,
                "media is moof/mdat": set(kinds[2:]) == {"moof", "mdat"},
                "size is the target": out_size == tuple(displays) == (1280, 720),
                # mean error in 8-bit steps: about 1 when right, 3.5 with the
                # wrong matrix, 7 with the wrong range
                "same picture": diff.mean() < 2.0,
            }
            ok &= all(checks.values())
            bad = [k for k, v in checks.items() if not v]
            print(f"segment {idx}: {src_size[0]}x{src_size[1]} -> {out_size[0]}x{out_size[1]}, "
                  f"{len(out_pts)} frames, pts {float(src_pts[0]):.3f}..{float(src_pts[-1]):.3f} s, "
                  f"tfdt {float(Fraction(tfdt, ts)):.3f} s, colour error {diff.mean():.2f}, "
                  f"{meta['ms'] / meta['frames']:.1f} ms/frame ({dt:.2f} s round trip), "
                  f"{len(out) / len(src):.1f}x the bytes  "
                  f"{'ok' if not bad else 'FAILED: ' + ', '.join(bad)}")
        same = len(inits) == 1
        ok &= same
        print(f"output init segment identical across segments: {same}")

        # In parts: the same frames, and the first of them early.
        src = init + chunks[1]
        whole = post(src)[1]
        status, parts, meta, first, last = post_in_parts(src)
        joined = b"".join(parts)
        expect("in parts: the same frames at the same times",
               (status, packets(joined) == packets(whole)), (200, True))
        expect("in parts: more than one, starting with the init segment and 0.4 s of video",
               (len(parts) > 1, parts[0][:meta["init_len"]] in inits,
                len(packets(parts[0])[0]) >= 12, meta["frames"]),
               (True, True, True, 60))
        print(f"  {len(parts)} parts, the first after {first * 1000:.0f} ms, the last after {last * 1000:.0f} ms")
        status, parts, meta, _, _ = post_in_parts(src, "&behind=1.5&rate=2")
        expect("behind the playhead and double speed: still the same frames at the same times",
               (status, packets(b"".join(parts)) == packets(whole)), (200, True))
        expect("a fragment nobody is fetching has no next part", call("/transform/next?job=12345")[0], 410)

        # A screen that is none of this machine's displays changes nothing.
        status, out, meta = post(init + chunks[1], "?sw=1024&sh=600&dpr=1")
        expect("unknown screen keeps the target", (status, meta and meta["out"]), (200, "1280x720"))
        status, out, meta = post(a_init + a_chunks[0])
        expect("audio passes through", (status, bool(meta and meta["passthrough"])), (204, True))
        status, out, meta = post(hd_init + hd_chunks[0])
        expect("720p at a 720p target passes through", (status, bool(meta and meta["passthrough"])), (204, True))
        status, out, meta = post(b"\0" * 64)
        expect("junk passes through", status, 204)
        status, out, meta = post(init + chunks[0][:len(chunks[0]) // 2])
        expect("a truncated fragment is refused, not mangled", status in (204, 500), True)
        status, out, meta = post(init + chunks[0], origin="https://example.com")
        expect("a web page is refused", status, 403)
        status, out, meta = post(init + chunks[0])
        expect("and the proxy still works afterwards", status, 200)

        # A page says upscaled video is playing by adding its screen to /config.
        expect("nothing is said to be playing", get("/status")["watching"], False)
        get("/config?playing=1&sw=1920&sh=1080&dpr=1")
        expect("a page's word that video is playing is kept", get("/status")["watching"], True)

        st = get("/status")
        print(f"status: {st['total_transforms']} segments, {st['total_frames']} frames, "
              f"last {st['last_in']} -> {st['last_out']} {st['last_mode']}, {st['fps'] or 0:.0f} fps")
    finally:
        stop_app(app, config)

    # With no resolution chosen the target is the largest display, and a page
    # reporting a smaller display's screen is held to that display.
    app, config = start_app({"quality": 1})
    try:
        wait_up(app)
        cfg = get("/config")
        if not cfg["displays"]:
            print("no displays detected (headless session?): per-display cap not checked")
        for d in cfg["displays"]:
            box = (min(d["width"], cfg["target"][0]), min(d["height"], cfg["target"][1]))
            want = "%dx%d" % plan((640, 360), box)
            query = f"?sw={d['width'] / d['scale']}&sh={d['height'] / d['scale']}&dpr={d['scale']}"
            status, out, meta = post(init + chunks[0], query)
            expect(f"page on {d['name']} ({d['width']}x{d['height']}), target {cfg['target'][0]}x{cfg['target'][1]}",
                   (status, meta and meta["out"]), (200, want))

        # A long fragment that is called off after its first part (the player
        # seeked away): the work stops, and the next fragment does not wait.
        long = init + b"".join(chunks)
        t0 = time.time()
        post(long)
        whole = time.time() - t0
        before = get("/status")["total_frames"]
        status, part, meta, job = call("/transform?parts=1", long)
        t0 = time.time()
        call(f"/transform/cancel?job={job}")
        gone = call(f"/transform/next?job={job}")[0]
        status, out, meta = post(init + chunks[0])
        waited = time.time() - t0
        expect("a fragment that was called off is gone", (gone, status), (410, 200))
        expect("and was not finished", get("/status")["total_frames"] - before, meta["frames"])
        print(f"  the whole of it takes {whole:.2f} s; the fragment after the cancelled one came in {waited:.2f} s")
        ok &= waited < whole / 2
    finally:
        stop_app(app, config)
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
