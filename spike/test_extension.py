#!/usr/bin/env python3
"""End-to-end test of extension/ + proxy in a real (headless) Chromium.

Serves a small MSE player page with a DASH stream built from the sample clip
(640x360 H.264 with B-frames, plus audio), starts the proxy, loads the unpacked
extension into a throwaway browser profile and checks that

  - the page plays 1280x720 video (it appended 640x360),
  - the whole timeline is buffered and playback advances,
  - the page saw normal SourceBuffer behaviour (updating / updateend / abort),
  - segments arriving in arbitrary pieces work too,
  - VP9 is reported as unsupported (codec steering), audio is untouched.

Usage:  ~/vsr-venv/bin/python spike/test_extension.py [browser command ...]
  default browser: the Vivaldi Flatpak, with a throwaway profile on disk
  needs: uv pip install --python ~/vsr-venv websockets
"""
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
import time
import urllib.request

import imageio_ffmpeg
import websockets

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
EXT = os.path.join(REPO, "extension")
PROXY_PORT, PAGE_PORT, CDP_PORT = 8730, 8731, 9333

PAGE = r"""<!doctype html><meta charset=utf-8><title>vsr mse test</title>
<video id=v muted playsinline></video>
<script>
(async () => {
  const R = window.__result = { segs: [], errors: [], events: {}, done: false };
  const q = new URLSearchParams(location.search);
  const piece = +q.get("piece") || 0, N = +q.get("n"), audio = q.get("audio") === "1";
  const v = document.getElementById("v");
  try {
    v.addEventListener("error", () => R.errors.push("video: " + (v.error && v.error.message)));
    const ms = new MediaSource();
    v.src = URL.createObjectURL(ms);
    await new Promise((r) => ms.addEventListener("sourceopen", r, { once: true }));
    R.vp9 = MediaSource.isTypeSupported('video/webm; codecs="vp9"');
    R.screenH = Math.round(screen.height * (window.devicePixelRatio || 1));
    R.hooked = !!window.__vsr;
    const sb = ms.addSourceBuffer('video/mp4; codecs="avc1.64001e"');
    const asb = audio ? ms.addSourceBuffer('audio/mp4; codecs="mp4a.40.2"') : null;
    for (const n of ["updatestart", "update", "updateend", "abort", "error"]) {
      sb.addEventListener(n, () => { R.events[n] = (R.events[n] || 0) + 1; });
    }
    let appends = 0;
    async function append(b, buf) {
      b.appendBuffer(buf);
      if (b === sb) appends++;
      if (!b.updating) R.errors.push("updating was false right after appendBuffer");
      await new Promise((r) => b.addEventListener("updateend", r, { once: true }));
      if (b.updating) R.errors.push("updating still true at updateend");
    }
    async function feed(b, url) {
      const buf = await (await fetch(url)).arrayBuffer();
      if (!piece || b !== sb) return append(b, buf);
      for (let o = 0; o < buf.byteLength; o += piece) await append(b, buf.slice(o, o + piece));
    }
    const name = (s, i) => `chunk-stream${s}-${String(i).padStart(5, "0")}.m4s`;
    await feed(sb, "init-stream0.m4s");
    if (asb) await feed(asb, "init-stream1.m4s");
    // A headless window that loses focus gets "video-only background media
    // was paused"; that is the browser, not the stream — just play again.
    const play = () => v.play().catch((e) =>
      e.name === "AbortError" ? setTimeout(play, 100) : R.errors.push("play: " + e));
    play();
    for (let i = 1; i <= N; i++) {
      await feed(sb, name(0, i));
      if (asb) await feed(asb, name(1, i));
      R.segs.push({ w: v.videoWidth, h: v.videoHeight,
                    end: sb.buffered.length ? sb.buffered.end(sb.buffered.length - 1) : 0 });
    }
    R.appends = appends;

    // While a fragment is away being transformed the buffer must look busy,
    // refuse remove(), and abort() must cancel it cleanly.
    const again = await (await fetch(name(0, 1))).arrayBuffer();
    const before = Object.assign({}, R.events);
    sb.appendBuffer(again);
    try { sb.remove(0, 1); R.errors.push("remove() during an append did not throw"); }
    catch (e) { if (e.name !== "InvalidStateError") R.errors.push("remove threw " + e.name); }
    sb.abort();
    if (sb.updating) R.errors.push("updating true after abort");
    await new Promise((r) => setTimeout(r, 300));
    R.abortEvents = { abort: (R.events.abort || 0) - (before.abort || 0),
                      updateend: (R.events.updateend || 0) - (before.updateend || 0) };
    await append(sb, again);          // usable again afterwards

    ms.endOfStream();
    R.buffered = []; for (let i = 0; i < v.buffered.length; i++) R.buffered.push([v.buffered.start(i), v.buffered.end(i)]);
    R.duration = v.duration;
    const t0 = performance.now();
    while (v.currentTime < 2 && performance.now() - t0 < 15000) await new Promise((r) => setTimeout(r, 100));
    R.currentTime = v.currentTime;
    R.size = [v.videoWidth, v.videoHeight];
    const q2 = v.getVideoPlaybackQuality();
    R.frames = { total: q2.totalVideoFrames, dropped: q2.droppedVideoFrames };
    R.vsr = window.__vsr ? window.__vsr.status() : null;
  } catch (e) {
    R.errors.push(String(e && e.stack || e));
  }
  R.done = true;
})();
</script>
"""


def make_site(dst):
    subprocess.run([
        imageio_ffmpeg.get_ffmpeg_exe(), "-y", "-loglevel", "error",
        "-i", os.path.join(HERE, "sample_720p.mp4"),
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
        "-map", "0:v:0", "-map", "1:a:0", "-shortest",
        "-vf", "scale=640:360", "-c:v", "libx264", "-preset", "veryfast",
        "-profile:v", "high", "-bf", "3", "-g", "60", "-keyint_min", "60",
        "-sc_threshold", "0", "-c:a", "aac", "-b:a", "96k",
        "-f", "dash", "-seg_duration", "2", "-use_template", "1",
        "-use_timeline", "0", os.path.join(dst, "manifest.mpd")], check=True)
    with open(os.path.join(dst, "index.html"), "w") as f:
        f.write(PAGE)
    n = len([f for f in os.listdir(dst) if f.startswith("chunk-stream0")])
    audio = os.path.exists(os.path.join(dst, "init-stream1.m4s"))
    return n, audio


class CDP:
    def __init__(self, ws):
        self.ws, self.n = ws, 0

    async def call(self, method, params=None, session=None):
        self.n += 1
        msg = {"id": self.n, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        await self.ws.send(json.dumps(msg))
        try:
            async with asyncio.timeout(30):
                while True:
                    r = json.loads(await self.ws.recv())
                    if r.get("id") == self.n:
                        if "error" in r:
                            raise RuntimeError(f"{method}: {r['error']}")
                        return r["result"]
        except TimeoutError:
            raise RuntimeError(f"{method}: no reply from the browser in 30 s") from None

    async def attach(self, target_id):
        return (await self.call("Target.attachToTarget",
                                {"targetId": target_id, "flatten": True}))["sessionId"]

    async def evaluate(self, session, expr):
        r = await self.call("Runtime.evaluate", {
            "expression": expr, "awaitPromise": True, "returnByValue": True}, session)
        if "exceptionDetails" in r:
            raise RuntimeError(r["exceptionDetails"].get("exception", {}).get("description")
                               or str(r["exceptionDetails"]))
        return r["result"].get("value")


def wait_http(url, timeout=60):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            return urllib.request.urlopen(url, timeout=2).read()
        except Exception:
            time.sleep(0.3)
    raise RuntimeError(f"timeout waiting for {url}")


async def run_page(cdp, url, timeout=120):
    # A new window: Vivaldi's default new tab never becomes scriptable headless.
    tid = (await cdp.call("Target.createTarget", {"url": url, "newWindow": True}))["targetId"]
    await asyncio.sleep(1.5)      # attach after the navigation has committed
    s = await cdp.attach(tid)
    t0 = time.time()
    res = None
    while time.time() - t0 < timeout:
        res = json.loads(await cdp.evaluate(s, "JSON.stringify(window.__result || null)"))
        if res and res["done"]:
            break
        await asyncio.sleep(0.5)
    await cdp.call("Target.closeTarget", {"targetId": tid})
    return res


def check(label, res, n, expect_vsr):
    # The extension caps the output at the screen height (600 px headless).
    f = min(2.0, res.get("screenH", 720) / 360) if expect_vsr else 1.0
    want = [max(2, round(640 * f / 2) * 2), max(2, round(360 * f / 2) * 2)]
    checks = {
        "no errors": not res["errors"],
        f"video is {want[0]}x{want[1]}": res.get("size") == want,
        "timeline buffered exactly": bool(res.get("buffered")) and len(res["buffered"]) == 1
                                     and abs(res["buffered"][0][0]) < 0.001
                                     and abs(res["buffered"][0][1] - 2 * n) < 0.001,
        "playback advances": (res.get("currentTime") or 0) >= 2,
        "one updateend per append": res["events"].get("updateend", 0) >= res.get("appends", 1e9),
        "abort() -> abort + updateend": res.get("abortEvents") == {"abort": 1, "updateend": 1}
                                        or not expect_vsr,
        "hook present / vp9 steered": (res.get("hooked"), res.get("vp9")) == (expect_vsr, not expect_vsr),
        "every segment transformed": not expect_vsr
                                     or (res["vsr"]["transformed"] >= n and res["vsr"]["passthrough"] == 0),
    }
    bad = [k for k, v in checks.items() if not v]
    print(f"{label}: size={res.get('size')} buffered={res.get('buffered')} "
          f"t={res.get('currentTime')} frames={res.get('frames')} vsr={res.get('vsr')}")
    print(f"  -> {'ok' if not bad else 'FAILED: ' + ', '.join(bad)}")
    for e in res["errors"]:
        print("     error:", e)
    return not bad


async def main():
    site = tempfile.mkdtemp(prefix="vsr-site-")
    profile = tempfile.mkdtemp(prefix="vsr-ext-profile-")
    # Flatpak: --die-with-parent, or the sandboxed browser outlives terminate();
    # and the profile dir is shared in from the host, because a sandbox's own
    # /tmp lives on the small /run/user tmpfs and a browser profile fills it.
    browser = sys.argv[1:] or ["flatpak", "run", "--die-with-parent",
                               f"--filesystem={EXT}:ro", f"--filesystem={profile}",
                               "com.vivaldi.Vivaldi"]
    n, audio = make_site(site)
    print(f"test stream: {n} segments of 2 s, audio={audio}")

    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *a):
            pass

    handler = functools.partial(Quiet, directory=site)
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", PAGE_PORT), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    proxy = subprocess.Popen([sys.executable, os.path.join(REPO, "proxy", "vsr_proxy.py"),
                              "--port", str(PROXY_PORT)],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    chrome = None
    ok = False
    try:
        wait_http(f"http://127.0.0.1:{PROXY_PORT}/health")
        chrome = subprocess.Popen(browser + [
            "--headless=new", "--no-first-run", f"--user-data-dir={profile}",
            f"--remote-debugging-port={CDP_PORT}", f"--load-extension={EXT}",
            "--disable-features=DisableLoadExtensionCommandLineSwitch",
            "--autoplay-policy=no-user-gesture-required",
            "--disable-background-media-suspend", "--disable-renderer-backgrounding",
            "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        ver = json.loads(wait_http(f"http://127.0.0.1:{CDP_PORT}/json/version"))
        print("browser:", ver["Browser"])
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
                print("FAIL: extension service worker not found — did the browser "
                      "ignore --load-extension?")
                return 1
            page = f"http://127.0.0.1:{PAGE_PORT}/index.html?n={n}&audio={int(audio)}"

            # 1. site not enabled: nothing may change
            s = await cdp.attach(sw["targetId"])
            for _ in range(40):       # the target exists before its script has run
                if await cdp.evaluate(s, "typeof syncScripts === 'function'"):
                    break
                await asyncio.sleep(0.25)
            await cdp.evaluate(s, "syncScripts()")
            ok = check("control (site off)   ", await run_page(cdp, page), n, False)

            # 2. site enabled
            await cdp.evaluate(s, """chrome.storage.local.set({vsrConfig:
                {sites: {"127.0.0.1": true}, mode: 2, scale: 2}}).then(syncScripts)""")
            ok &= check("whole segments       ", await run_page(cdp, page), n, True)
            ok &= check("64 kB pieces         ", await run_page(cdp, page + "&piece=65536"), n, True)
            await cdp.call("Browser.close")

        st = json.loads(wait_http(f"http://127.0.0.1:{PROXY_PORT}/status"))
        print(f"proxy: {st['total_transforms']} segments, {st['total_frames']} frames, "
              f"last {st['last_in']} -> {st['last_out']} {st['last_mode']}")
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
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
