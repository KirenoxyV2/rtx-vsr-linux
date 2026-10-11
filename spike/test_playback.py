#!/usr/bin/env python3
"""Does video play without stalling while it is being upscaled?

Plays streams cut the way YouTube cuts them (5 s segments, H.264 with
B-frames) through the extension and the app in a real Chromium, with the app
set to the most expensive mode (ULTRA, 2560x1440), and a page that behaves
like a player: it keeps some seconds buffered in front of the playhead,
appends one segment after the other, and after a seek starts again from the
segment under the playhead. For each stream it measures

  - how long the first frame takes,
  - how long playback stood still waiting for data after that,
  - how long a seek into video that is not buffered takes,
  - the frames the browser dropped and those it showed late (only a window
    on a real display tells), and the mode the app settled on.

The app has to be faster than playback for any of this to work, so the test
fails if the video stalls, or if starting or seeking takes seconds.

Usage:  ~/vsr-venv/bin/python spike/test_playback.py [options] [path/to/rtx-vsr]
  --streams 720p60,1080p30,1080p60,1080p30x2   which of the streams to play
                                     (x2: at double speed)
  --quality 4                        the app's quality setting
  --window                           in a window on this desktop instead of headless
  --off                              upscaling switched off, to compare with
  --play 8 --ahead 8                 seconds played before the seek, and seconds
                                     the page keeps buffered (a long session:
                                     --play 60 --ahead 30)
  needs what spike/test_extension.py needs.
"""
import argparse
import asyncio
import functools
import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading

import imageio_ffmpeg
import websockets

from test_extension import BROWSER, CDP, connected_command, wait_http

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
# Not the app's usual port, nor the other tests': they may be running.
PROXY_PORT, PAGE_PORT, CDP_PORT = 8747, 8733, 9335
SEGMENT = 5                       # seconds, as on YouTube
STREAMS = {"480p30": (854, 480, 30), "720p30": (1280, 720, 30), "720p60": (1280, 720, 60),
           "1080p30": (1920, 1080, 30), "1080p60": (1920, 1080, 60)}
# What counts as playing well: seconds until the first frame, seconds stalled
# afterwards, seconds a seek into unbuffered video takes.
LIMITS = {"start": 2.0, "stalled": 0.25, "seek": 3.5}

PAGE = r"""<!doctype html><meta charset=utf-8><title>vsr playback test</title>
<style>html,body{margin:0;background:#000;height:100%}video{width:100vw;height:100vh}</style>
<video id=v muted playsinline></video>
<script>
// Started by the test once it is attached to the page: attaching is felt.
window.run = async () => {
  const R = window.__result = { errors: [], done: false, log: [] };
  const born = performance.now();
  const note = (...what) => R.log.push([Math.round(performance.now() - born), ...what]);
  const q = new URLSearchParams(location.search);
  const N = +q.get("n"), SEG = +q.get("seg"), AHEAD = +q.get("ahead"), PLAY = +q.get("play") * 1000;
  const RATE = +q.get("rate") || 1;
  const v = document.getElementById("v");
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  try {
    v.addEventListener("error", () => R.errors.push("video: " + (v.error && v.error.message)));
    // Everything is fetched first: the upscaling is what gets measured.
    const get = async (u) => (await fetch(u)).arrayBuffer();
    const init = await get("init-stream0.m4s"), segs = [];
    for (let i = 1; i <= N; i++) segs.push(await get(`chunk-stream0-${String(i).padStart(5, "0")}.m4s`));
    const ms = new MediaSource();
    v.src = URL.createObjectURL(ms);
    await new Promise((r) => ms.addEventListener("sourceopen", r, { once: true }));
    ms.duration = N * SEG;
    const sb = ms.addSourceBuffer('video/mp4; codecs="avc1.640028"');
    R.hooked = !!window.__vsr;

    // The player: AHEAD seconds in front of the playhead, segment by segment.
    let next = 0, stop = false;
    const ranges = () => Array.from({ length: v.buffered.length },
      (_, i) => [+v.buffered.start(i).toFixed(2), +v.buffered.end(i).toFixed(2)]);
    const append = (buf, what) => new Promise((resolve) => {
      note("append", what);
      sb.addEventListener("updateend", () => { note("appended", what, ranges()); resolve(); }, { once: true });
      try { sb.appendBuffer(buf); } catch (e) { R.errors.push("append: " + e); resolve(); }
    });
    for (const name of ["waiting", "playing", "seeking", "seeked", "pause", "stalled", "ended"]) {
      v.addEventListener(name, () => note(name, +v.currentTime.toFixed(2), v.readyState, ranges()));
    }
    const feeding = (async () => {
      await append(init, "init");
      while (!stop) {
        if (next >= N || next * SEG - v.currentTime > AHEAD) { await sleep(20); continue; }
        await append(segs[next], next++);
      }
    })();
    v.addEventListener("seeking", () => {
      if (sb.updating) sb.abort();
      next = Math.min(N - 1, Math.floor(v.currentTime / SEG));
    });

    // Standing still although it should be playing: that is a stall.
    let stalled = 0, waits = 0, counting = false, last = performance.now();
    const moving = () => !v.paused && !v.ended && !v.seeking && v.readyState > 2;
    const watch = setInterval(() => {
      const now = performance.now();
      if (counting && !moving()) stalled += now - last;
      last = now;
    }, 10);
    v.addEventListener("waiting", () => { if (counting) waits++; });
    const sampling = setInterval(() => {
      const quality = v.getVideoPlaybackQuality(), b = ranges().find(([from, to]) => from <= v.currentTime && v.currentTime < to);
      note("at", +v.currentTime.toFixed(2), "lead", b ? +(b[1] - v.currentTime).toFixed(2) : 0,
           "frames", quality.totalVideoFrames, "dropped", quality.droppedVideoFrames);
    }, 500);
    if (q.get("trace")) {
      // What exactly happens when: parts arriving, frames dropped, frames late.
      let end = 0, dropped = 0, shown = 0;
      setInterval(() => {
        const r = ranges(), e = r.length ? r[0][1] : 0, d = v.getVideoPlaybackQuality().droppedVideoFrames;
        if (e !== end) note("buffered to", e, "playhead", +v.currentTime.toFixed(2));
        if (d !== dropped) note("dropped", d - dropped, "playhead", +v.currentTime.toFixed(2));
        end = e; dropped = d;
      }, 15);
      const each = (now, md) => {
        if (shown && md.expectedDisplayTime - shown > 40) note("frame late by", Math.round(md.expectedDisplayTime - shown), "at", +md.mediaTime.toFixed(2));
        shown = md.expectedDisplayTime;
        v.requestVideoFrameCallback(each);
      };
      v.requestVideoFrameCallback(each);
    }
    // When each frame got on screen, against when the one before it did:
    // [ms between them, ms of video between them, time in the video].
    R.gaps = [];
    let before = null;
    const shownAt = (now, md) => {
      if (counting && before) {
        R.gaps.push([+(md.expectedDisplayTime - before.expectedDisplayTime).toFixed(1),
                     +((md.mediaTime - before.mediaTime) * 1000 / RATE).toFixed(1), +md.mediaTime.toFixed(2)]);
      }
      before = counting ? md : null;
      v.requestVideoFrameCallback(shownAt);
    };
    v.requestVideoFrameCallback(shownAt);
    const frame = () => new Promise((r) => v.requestVideoFrameCallback(r));
    const span = async (ms) => {
      const from = { t: v.currentTime, stalled, waits }, t0 = performance.now();
      await sleep(ms);
      return { wall: (performance.now() - t0) / 1000, played: v.currentTime - from.t,
               stalled: (stalled - from.stalled) / 1000, waits: waits - from.waits };
    };

    const t0 = performance.now();
    v.playbackRate = RATE;
    v.play().catch((e) => R.errors.push("play: " + e));
    await frame();
    while (!moving()) await sleep(5);
    R.start = (performance.now() - t0) / 1000;
    counting = true;
    R.first = await span(PLAY / RATE);

    // Into the middle of a segment that has not been appended yet.
    const to = (N - 1) * SEG + SEG / 2;
    counting = false;
    const t1 = performance.now();
    v.currentTime = to;
    await sleep(0);
    while (!moving() || v.currentTime < to) await sleep(5);
    R.seek = { to, took: (performance.now() - t1) / 1000 };
    counting = true;
    R.second = await span((N * SEG - to - 0.5) * 1000 / RATE);

    counting = false;
    stop = true;
    clearInterval(watch);
    clearInterval(sampling);
    const quality = v.getVideoPlaybackQuality();
    R.frames = { total: quality.totalVideoFrames, dropped: quality.droppedVideoFrames };
    R.size = [v.videoWidth, v.videoHeight];
    R.vsr = window.__vsr ? window.__vsr.status() : null;
  } catch (e) {
    R.errors.push(String(e && e.stack || e));
  }
  R.done = true;
};
</script>
"""


def make_stream(dst, name, segments):
    """The sample clip, over and over, at the size and rate `name` says."""
    w, h, fps = STREAMS[name]
    os.makedirs(dst, exist_ok=True)
    vf = f"scale={w}:{h}:flags=lanczos" + (f",framerate=fps={fps}" if fps != 30 else "")
    subprocess.run([
        imageio_ffmpeg.get_ffmpeg_exe(), "-y", "-loglevel", "error", "-stream_loop", "-1",
        "-i", os.path.join(HERE, "sample_720p.mp4"), "-an", "-vf", vf,
        "-frames:v", str(fps * SEGMENT * segments),
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "21", "-profile:v", "high", "-bf", "2",
        "-g", str(fps * SEGMENT), "-keyint_min", str(fps * SEGMENT), "-sc_threshold", "0",
        "-f", "dash", "-seg_duration", str(SEGMENT), "-use_template", "1", "-use_timeline", "0",
        os.path.join(dst, "manifest.mpd")], check=True)
    with open(os.path.join(dst, "index.html"), "w") as f:
        f.write(PAGE)
    count = sum(1 for f in os.listdir(dst) if f.startswith("chunk-stream0"))
    assert count == segments, f"{name}: {count} segments"


async def play(cdp, url, timeout=120):
    tid = (await cdp.call("Target.createTarget", {"url": url, "newWindow": True}))["targetId"]
    await asyncio.sleep(1.5)      # attach after the navigation has committed
    s = await cdp.attach(tid)
    await asyncio.sleep(1)
    await cdp.evaluate(s, "void window.run()")
    res = None
    for _ in range(timeout * 2):
        res = json.loads(await cdp.evaluate(s, "JSON.stringify(window.__result || null)"))
        if res and res["done"]:
            break
        await asyncio.sleep(0.5)
    await cdp.call("Target.closeTarget", {"targetId": tid})
    return res


def hitches(res):
    """Frames that got on screen late: later, after the one before, than the
    video says by more than half a frame (and by more than a display may add
    by itself). -> (how many, the worst in ms, of how many frames)"""
    gaps = res.get("gaps") or []
    late = [wall - media for wall, media, _ in gaps
            if media > 0 and wall - media > max(12.0, media / 2)]
    return len(late), max(late, default=0.0), len(gaps)


def report(name, res, upscaling=True):
    if not res or not res.get("done") or res["errors"] or "second" not in res:
        print(f"{name:9s} FAILED: {res and res['errors'] or 'the page did not finish'}")
        return False
    a, b, vsr = res["first"], res["second"], res.get("vsr") or {}
    stalled = a["stalled"] + b["stalled"]
    last = vsr.get("last") or {}
    bad = [k for k, v in (("start", res["start"]), ("stalled", stalled), ("seek", res["seek"]["took"]))
           if v > LIMITS[k]]
    if upscaling and (res["size"] != [2560, 1440] or not vsr.get("transformed") or vsr.get("passthrough")):
        bad.append("not upscaled")
    late, worst, shown = hitches(res)
    print(f"{name:9s} first frame after {res['start']:5.2f} s | stalled {stalled:5.2f} s "
          f"({a['waits'] + b['waits']} times) in {a['played'] + b['played']:.0f} s of video | "
          f"seek to {res['seek']['to']:.1f} s took {res['seek']['took']:5.2f} s | "
          f"frames {res['frames']['total']}, dropped {res['frames']['dropped']}, "
          f"late {late} of {shown} (worst {worst:.0f} ms) | "
          f"{res['size'][0]}x{res['size'][1]} {last.get('mode_name', 'not upscaled')}"
          f"{'' if not bad else '   FAILED: ' + ', '.join(bad)}")
    return not bad


async def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("app", nargs="?")
    ap.add_argument("--streams", default="720p60,1080p30,1080p60,1080p30x2")
    ap.add_argument("--quality", type=int, default=4)
    ap.add_argument("--window", action="store_true")
    ap.add_argument("--play", type=int, default=8, help="seconds to play before the seek")
    ap.add_argument("--ahead", type=int, default=8, help="seconds the page keeps buffered")
    ap.add_argument("--off", action="store_true",
                    help="with upscaling switched off in the app: what the browser does by itself")
    ap.add_argument("--log", action="store_true", help="print what the page saw, event by event")
    args = ap.parse_args()
    app = args.app or next((p for p in (os.path.join(REPO, "app", "target", b, "rtx-vsr")
                                         for b in ("release", "debug")) if os.path.exists(p)), None)
    if not app:
        sys.exit("build the app first: (cd app && cargo build --release)")

    site = tempfile.mkdtemp(prefix="vsr-play-site-")
    profile = tempfile.mkdtemp(prefix="vsr-play-profile-")
    home = tempfile.mkdtemp(prefix="vsr-play-home-")
    argv, ext = connected_command(app, home)
    run = argv.index("run") + 1
    browser = argv[:run] + ["--die-with-parent", f"--filesystem={ext}:ro",
                            f"--filesystem={profile}"] + argv[run:]
    names = args.streams.split(",")
    # Enough for the seek to land in a segment that is not buffered yet.
    segments = (args.play + args.ahead) // SEGMENT + 3
    for name in {name.split("x")[0] for name in names}:
        make_stream(os.path.join(site, name), name, segments)

    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *a):
            pass

    httpd = http.server.ThreadingHTTPServer(
        ("127.0.0.1", PAGE_PORT), functools.partial(Quiet, directory=site))
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    config = os.path.join(home, "app-config")
    os.makedirs(os.path.join(config, "rtx-vsr"))
    with open(os.path.join(config, "rtx-vsr", "config.json"), "w") as f:
        json.dump({"target": [2560, 1440], "quality": args.quality, "enabled": not args.off}, f)
    proxy = subprocess.Popen([app, "--headless", "--port", str(PROXY_PORT)],
                             env=dict(os.environ, XDG_CONFIG_HOME=config),
                             stdout=subprocess.DEVNULL,
                             stderr=None if args.log else subprocess.DEVNULL)
    chrome = None
    ok = False
    try:
        wait_http(f"http://127.0.0.1:{PROXY_PORT}/health")
        chrome = subprocess.Popen(browser + ([] if args.window else ["--headless=new"]) + [
            "--no-first-run", f"--user-data-dir={profile}",
            f"--remote-debugging-port={CDP_PORT}",
            "--autoplay-policy=no-user-gesture-required",
            "--disable-background-media-suspend", "--disable-renderer-backgrounding",
            "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        ver = json.loads(wait_http(f"http://127.0.0.1:{CDP_PORT}/json/version"))
        print(f"browser: {ver['Browser']}{'' if args.window else ' (headless)'}; "
              f"app asked for {['', 'LOW', 'MEDIUM', 'HIGH', 'ULTRA'][args.quality]} at 2560x1440")
        async with websockets.connect(ver["webSocketDebuggerUrl"], max_size=None) as ws:
            cdp = CDP(ws)
            sw = None
            for _ in range(40):
                targets = (await cdp.call("Target.getTargets"))["targetInfos"]
                sw = next((t for t in targets if t["type"] == "service_worker"
                           and t["url"].endswith("/background.js")), None)
                if sw:
                    break
                await asyncio.sleep(0.25)
            if not sw:
                print("FAIL: the browser did not load the extension")
                return 1
            s = await cdp.attach(sw["targetId"])
            for _ in range(40):       # the target exists before its script has run
                if await cdp.evaluate(s, "typeof syncScripts === 'function'"):
                    break
                await asyncio.sleep(0.25)
            await cdp.evaluate(s, """chrome.storage.local.set({vsrConfig:
                {sites: {"127.0.0.1": true}, proxy: "http://127.0.0.1:%d"}}).then(syncScripts)""" % PROXY_PORT)
            ok = True
            for name in names:
                stream, _, rate = name.partition("x")
                url = (f"http://127.0.0.1:{PAGE_PORT}/{stream}/index.html"
                       f"?n={segments}&seg={SEGMENT}&ahead={args.ahead}&play={args.play}"
                       f"&rate={rate or 1}{'&trace=1' if args.log else ''}")
                res = await play(cdp, url, 120 + 2 * args.play)
                ok &= report(name, res, not args.off)
                if args.log:
                    for entry in (res or {}).get("log", []):
                        print("   ", *entry)
            await cdp.call("Browser.close")
    finally:
        for p in (chrome, proxy):
            if p:
                p.terminate()
        httpd.shutdown()
        shutil.rmtree(site, ignore_errors=True)
        if chrome:
            chrome.wait(timeout=10)
        # Chromium's zygotes run in nested sandboxes and can outlive the browser.
        subprocess.run(["pkill", "-f", f"user-data-dir={profile}"])
        shutil.rmtree(profile, ignore_errors=True)
        shutil.rmtree(home, ignore_errors=True)
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
