#!/usr/bin/env python3
"""End-to-end test of extension/ + the app in a real (headless) Chromium.

Serves a small MSE player page with a DASH stream built from the sample clip
(640x360 H.264 with B-frames, plus audio), starts the app's proxy without a
window, and starts the browser the way the app's "Connect <browser>" switch
makes it start: with the command line of the launcher override that
`rtx-vsr --connect` writes (into a throwaway home, so nothing of yours is
touched). Checks that

  - that command line is enough for a fresh profile to load the extension,
  - the page plays 1280x720 video (it appended 640x360),
  - the whole timeline is buffered and playback advances,
  - the page saw normal SourceBuffer behaviour (updating / updateend / abort),
  - segments arriving in arbitrary pieces work too,
  - VP9 is reported as unsupported (codec steering), audio is untouched,
  - the "RTX VSR" indicator sits in the video's top right corner when the
    app's setting is on (its picture loaded, although the page's content
    security policy forbids images and inline styles), and is absent when
    it is off.

Usage:  ~/vsr-venv/bin/python spike/test_extension.py [path/to/rtx-vsr]
  needs: the Vivaldi Flatpak (com.vivaldi.Vivaldi), a built app
         ((cd app && cargo build --release)), and
         uv pip install --python ~/vsr-venv websockets
"""
import asyncio
import functools
import http.server
import json
import os
import shlex
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
BROWSER = "com.vivaldi.Vivaldi"
# Not the app's usual port: yours may be running.
PROXY_PORT, PAGE_PORT, CDP_PORT = 8744, 8731, 9333


def find_app():
    if len(sys.argv) > 1:
        return sys.argv[1]
    for build in ("release", "debug"):
        p = os.path.join(REPO, "app", "target", build, "rtx-vsr")
        if os.path.exists(p):
            return p
    sys.exit("build the app first: (cd app && cargo build --release)")

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
    // The indicator is placed by a timer; give it a moment.
    const find = () => document.querySelector("[data-rtx-vsr-indicator]");
    const t1 = performance.now();
    const shown = () => find() && find().style.display === "block" && find().complete;
    while (q.get("mark") === "1" && !shown()
           && performance.now() - t1 < 4000) await new Promise((r) => setTimeout(r, 100));
    const m = find();
    R.mark = m && m.style.display === "block" ? {
      picture: m.naturalWidth, opacity: getComputedStyle(m).opacity, shadow: getComputedStyle(m).boxShadow,
      left: m.offsetLeft, top: m.offsetTop, side: [m.offsetWidth, m.offsetHeight],
      video: [v.offsetLeft, v.offsetTop, v.clientWidth, v.clientHeight],
    } : null;
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


def check(label, res, n, expect_vsr, expect_mark=False):
    # The app upscales to its configured 1280x720 (the headless window's
    # 800x600 screen is no display it knows, so there is no further cap).
    want = [1280, 720] if expect_vsr else [640, 360]
    mark = res.get("mark")
    if mark:
        vx, vy, vw, vh = mark["video"]
        in_corner = (mark["picture"] > 0 and mark["opacity"] == "0.7" and mark["shadow"] == "none"
                     and mark["side"][0] == mark["side"][1] > 20
                     and vx + vw / 2 < mark["left"] and mark["left"] + mark["side"][0] <= vx + vw
                     and vy <= mark["top"] < vy + vh / 4)
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
        "indicator in the top right corner" if expect_mark else "no indicator":
            bool(mark) and in_corner if expect_mark else not mark,
    }
    bad = [k for k, v in checks.items() if not v]
    print(f"{label}: size={res.get('size')} buffered={res.get('buffered')} "
          f"t={res.get('currentTime')} frames={res.get('frames')} vsr={res.get('vsr')} mark={mark}")
    print(f"  -> {'ok' if not bad else 'FAILED: ' + ', '.join(bad)}")
    for e in res["errors"]:
        print("     error:", e)
    return not bad


def start_app(app, config, indicator):
    """The proxy without a window, upscaling to 1280x720."""
    os.makedirs(os.path.join(config, "rtx-vsr"), exist_ok=True)
    with open(os.path.join(config, "rtx-vsr", "config.json"), "w") as f:
        json.dump({"target": [1280, 720], "quality": 2, "indicator": indicator}, f)
    proc = subprocess.Popen([app, "--headless", "--port", str(PROXY_PORT)],
                            env=dict(os.environ, XDG_CONFIG_HOME=config),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    wait_http(f"http://127.0.0.1:{PROXY_PORT}/health")
    return proc


def connected_command(app, home):
    """What the launcher override made by `rtx-vsr --connect` runs, as argv
    (without the desktop-entry placeholders), and the extension directory."""
    env = dict(os.environ, HOME=home, XDG_DATA_HOME=os.path.join(home, "data"),
               XDG_CONFIG_HOME=os.path.join(home, "config"))
    subprocess.run([app, "--connect", BROWSER], env=env, check=True, stdout=subprocess.DEVNULL)
    with open(os.path.join(home, "data", "applications", BROWSER + ".desktop")) as f:
        entry = f.read()
    assert "X-RTX-VSR-Managed=true" in entry
    exec_line = next(l[5:] for l in entry.splitlines() if l.startswith("Exec="))
    argv = [a for a in shlex.split(exec_line) if a not in ("@@u", "@@", "%U", "%u", "--file-forwarding")]
    ext = next(a.split("=", 1)[1] for a in argv if a.startswith("--load-extension="))
    return argv, ext


async def main():
    app = find_app()
    site = tempfile.mkdtemp(prefix="vsr-site-")
    profile = tempfile.mkdtemp(prefix="vsr-ext-profile-")
    home = tempfile.mkdtemp(prefix="vsr-ext-home-")
    argv, ext = connected_command(app, home)
    print("launcher runs:", " ".join(argv))
    # Flatpak: --die-with-parent, or the sandboxed browser outlives terminate();
    # the profile dir is shared in from the host, because a sandbox's own /tmp
    # lives on the small /run/user tmpfs and a browser profile fills it; and
    # the extension is shared in because this test keeps it in a throwaway
    # home instead of the browser's own data directory.
    run = argv.index("run") + 1
    browser = argv[:run] + ["--die-with-parent", f"--filesystem={ext}:ro",
                            f"--filesystem={profile}"] + argv[run:]
    n, audio = make_site(site)
    print(f"test stream: {n} segments of 2 s, audio={audio}")

    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def end_headers(self):
            # As strict as sites get about what the indicator is made of.
            self.send_header("Content-Security-Policy", "img-src 'none'; style-src 'none'")
            super().end_headers()

    handler = functools.partial(Quiet, directory=site)
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", PAGE_PORT), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    config = os.path.join(home, "app-config")
    proxy = start_app(app, config, indicator=False)
    chrome = None
    ok = False
    try:
        chrome = subprocess.Popen(browser + [
            "--headless=new", "--no-first-run", f"--user-data-dir={profile}",
            f"--remote-debugging-port={CDP_PORT}",
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
                print("FAIL: extension service worker not found — the launcher's "
                      "command line did not make the browser load the extension")
                return 1
            print("extension loaded as", sw["url"].split("/")[2])
            page = f"http://127.0.0.1:{PAGE_PORT}/index.html?n={n}&audio={int(audio)}"

            # 1. site not enabled: nothing may change
            s = await cdp.attach(sw["targetId"])
            for _ in range(40):       # the target exists before its script has run
                if await cdp.evaluate(s, "typeof syncScripts === 'function'"):
                    break
                await asyncio.sleep(0.25)
            await cdp.evaluate(s, "syncScripts()")
            ok = check("control (site off)       ", await run_page(cdp, page), n, False)

            # 2. site enabled; the app's indicator setting is off
            await cdp.evaluate(s, """chrome.storage.local.set({vsrConfig:
                {sites: {"127.0.0.1": true}, proxy: "http://127.0.0.1:%d"}}).then(syncScripts)""" % PROXY_PORT)
            ok &= check("whole segments           ", await run_page(cdp, page + "&mark=1"), n, True)

            # 3. the same with the indicator switched on in the app
            proxy.terminate()
            proxy.wait(timeout=10)
            proxy = start_app(app, config, indicator=True)
            ok &= check("64 kB pieces + indicator ", await run_page(cdp, page + "&piece=65536&mark=1"),
                        n, True, expect_mark=True)
            # the page was playing upscaled video until a moment ago, and said so
            watching = json.loads(wait_http(f"http://127.0.0.1:{PROXY_PORT}/status"))["watching"]
            print(f"app told that upscaled video is playing: {watching}")
            ok &= watching
            await cdp.call("Browser.close")

        st = json.loads(wait_http(f"http://127.0.0.1:{PROXY_PORT}/status"))
        print(f"app: {st['total_transforms']} segments, {st['total_frames']} frames, "
              f"last {st['last_in']} -> {st['last_out']} {st['last_mode']}, {st['fps'] or 0:.0f} fps")
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
