#!/usr/bin/env python3
"""
vsr_proxy.py — local RTX VSR proxy (route B core).

A tiny HTTP server that takes a video source, runs it through NVIDIA's VSR
model on the GPU, re-encodes to browser-playable H.264, and serves it back.
This is the connection-agnostic CORE: the browser extension just rewrites
media URLs to hit this endpoint; a future "point your whole browser here"
mode hits the same endpoint.

First increment (this file): FILE source + full-clip response + demo page.
  - GET /              demo UI (list local clips, click to upscale & play)
  - GET /health        liveness
  - GET /upscale?src=<abs path>&mode=2&scale=1.5   -> video/mp4 (H.264)

Pipeline per frame: cv2 decode (CPU) -> cupy H2D + format -> nvvfx VSR (GPU)
-> D2H -> libx264 encode (imageio-ffmpeg, CPU). The CPU I/O is the *pessimistic*
baseline; NVDEC+NVENC is the later measured upgrade. We already know from M1.5
the AI stage alone is 0.6 ms/frame, so even with CPU I/O this stays real-time
for typical clips on a 4090.

Run:
    source spike/env.sh
    .venv/bin/python proxy/vsr_proxy.py [--port 8730] [--gpu 0]
Then open http://127.0.0.1:8730/ in Brave/Firefox.

Env:
    VSR_GPU      default GPU index (default 0)
    VSR_CLIPS_DIR  dir to list on the demo page (default ~/VSR/spike)
"""
import argparse
import io
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

import cv2
import numpy as np

# GPU deps imported lazily per-request-ish so /health works even if CUDA is wedged.
GPU = {"ready": False, "err": None}
# Serialize GPU work: the browser fires multiple segment requests at once and
# they must not share the CUDA context / effect state concurrently.
GPU_LOCK = threading.Lock()


def _gpu_setup(gpu_index):
    import cupy as cp
    from nvvfx import VideoSuperRes
    dev = cp.cuda.Device(gpu_index)
    with dev:
        name = cp.cuda.runtime.getDeviceProperties(gpu_index)["name"].decode()
        free, total = cp.cuda.runtime.memGetInfo()
    GPU.update(ready=True, err=None, cupy=cp, VideoSuperRes=VideoSuperRes,
               gpu_index=gpu_index, name=name,
               free_mb=free // (1024 * 1024), total_mb=total // (1024 * 1024))


def _align16(n):
    return max(16, int(round(n / 16.0)) * 16)


class InsufficientVRAM(Exception):
    """Raised when no VSR mode fits in live free VRAM (or strict=1 is set)."""
    def __init__(self, free_mb, need_mb, label):
        self.free_mb = free_mb
        self.need_mb = need_mb
        super().__init__(
            f"not enough free VRAM: have {free_mb} MB, {label} needs ~{need_mb} MB. "
            f"Free some VRAM (e.g. close a local LLM server) or pick a lower scale.")


class ContextWedged(Exception):
    """Raised when the CUDA context is already poisoned and can't even be queried."""
    def __init__(self):
        super().__init__(
            "GPU context is wedged (a previous run hit an illegal memory access). "
            "Restart the proxy to recover.")


# VSR quality modes (nvvfx VideoSuperRes quality levels).
MODES = {1: "LOW", 2: "MEDIUM", 3: "HIGH", 4: "ULTRA"}

# Measured VRAM deltas from M1 (free_before - free_after during a run), at the
# largest resolution tested per mode (1080p->4K). We use these as a per-mode
# ceiling and scale up only for outputs larger than that reference.
_MODE_VRAM_MB = {1: 200, 2: 400, 3: 600, 4: 600}
_REF_OUT_MP = 3840 * 2160 / 1e6  # 8.29 MP (the M1 reference output)


def est_vram_mb(mode, in_w, in_h, out_w, out_h):
    """Conservative VRAM estimate for a VSR run (engine + buffers), in MB."""
    base = _MODE_VRAM_MB.get(mode, 600) * 1.3          # safety factor on measured delta
    out_mp = (out_w * out_h) / 1e6
    extra = max(0.0, out_mp - _REF_OUT_MP) * 72.0       # ~72 MB/MP above the reference
    overhead = 150                                       # process + transient headroom
    return int(base + extra + overhead)


def live_free_mb(gpu_index):
    """Current free VRAM on the GPU, or None if it can't be read (context wedged)."""
    try:
        cp = GPU["cupy"]
        with cp.cuda.Device(gpu_index):
            free, _total = cp.cuda.runtime.memGetInfo()
        return free // (1024 * 1024)
    except Exception:
        return None

def choose_mode(requested_mode, free_mb, out_w, out_h):
    """VRAM-aware auto-downshift: highest mode <= requested whose estimated
    VRAM fits in the live free_mb. Returns None if even LOW doesn't fit."""
    for m in sorted(MODES, reverse=True):
        if m > requested_mode:
            continue
        if est_vram_mb(m, 0, 0, out_w, out_h) <= free_mb:
            return m
    return None


def upscale_clip(src_path, mode, scale, gpu_index, progress=None, strict=False):
    """Run a whole clip through VSR; return (mp4_bytes, meta_dict)."""
    import imageio_ffmpeg

    cp = GPU["cupy"]
    VideoSuperRes = GPU["VideoSuperRes"]
    dev = cp.cuda.Device(gpu_index)

    cap = cv2.VideoCapture(src_path)
    if not cap.isOpened():
        raise RuntimeError(f"cannot open source: {src_path}")
    src_fps = cap.get(cv2.CAP_PROP_FPS) or 30.0
    src_w = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
    src_h = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
    out_w = _align16(src_w * scale)
    out_h = _align16(src_h * scale)

    # Pre-flight VRAM check + auto-downshift (before touching CUDA): never let
    # the engine OOM-fault and poison the context. Pick the highest quality
    # mode that fits in *live* free VRAM so the proxy degrades gracefully
    # while a local LLM shares the GPU, instead of failing outright.
    requested_mode = mode
    free_mb = live_free_mb(gpu_index)
    if free_mb is None:
        raise ContextWedged()
    if strict:
        need_mb = est_vram_mb(mode, src_w, src_h, out_w, out_h)
        if free_mb < need_mb:
            raise InsufficientVRAM(free_mb, need_mb, f"{MODES.get(mode, mode)} -> {out_w}x{out_h}")
    else:
        chosen = choose_mode(mode, free_mb, out_w, out_h)
        if chosen is None:
            low_need = est_vram_mb(1, src_w, src_h, out_w, out_h)
            raise InsufficientVRAM(free_mb, low_need, f"even LOW -> {out_w}x{out_h}")
        if chosen != mode:
            print(f"# downshift: {MODES[mode]} -> {MODES[chosen]} "
                  f"(free {free_mb} MB of {GPU['total_mb']} MB)", file=sys.stderr, flush=True)
        mode = chosen
    downshifted = mode != requested_mode

    with GPU_LOCK:  # serialize all GPU work across concurrent segment requests
      with dev:
        sr = VideoSuperRes(quality=mode, device=gpu_index)
        sr.output_width = out_w
        sr.output_height = out_h
        sr.load()

        # warmup (hold the result alive; clone output into independent memory)
        x_dummy = cp.random.rand(3, src_h, src_w, dtype=cp.float32)
        _res = sr.run(x_dummy)
        _img = cp.array(cp.from_dlpack(_res.image))  # copy out of effect-owned buffer
        dev.synchronize()
        del _res, _img, x_dummy

        tmp_path = os.path.join("/tmp", f"vsr_out_{os.getpid()}_{int(time.time())}.mp4")
        writer = imageio_ffmpeg.write_frames(
            tmp_path, (out_w, out_h), fps=int(round(src_fps)),
            codec="libx264", quality=None,
            pix_fmt_in="bgr24", macro_block_size=16,
            output_params=["-movflags", "+faststart", "-preset", "veryfast"],
        )
        writer.send(None)  # prime the generator

        n = 0
        t_start = time.perf_counter()
        while True:
            ok, frame = cap.read()
            if not ok:
                break
            # H2D + format to CHW float32 [0,1] RGB
            x = cp.asarray(frame[:, :, ::-1])
            x = cp.ascontiguousarray(cp.transpose(x, (2, 0, 1)).astype(cp.float32) / 255.0)
            # Hold the run result alive and COPY its image into independent
            # memory BEFORE it can be GC'd — from_dlpack() returns a view into
            # effect-owned GPU memory; freeing the temp corrupts the context.
            res = sr.run(x)
            out = cp.array(cp.from_dlpack(res.image))  # .array() => deep copy
            del res, x
            dev.synchronize()
            # D2H + normalize to BGR uint8 HWC
            o = out.get()
            del out
            if o.dtype in (np.float32, np.float16):
                o = (o * 255.0).round().astype(np.uint8)
            else:
                o = o.astype(np.uint8)
            if o.ndim == 3 and o.shape[0] in (1, 3, 4) and o.shape[0] < o.shape[1]:
                o = np.ascontiguousarray(o.transpose(1, 2, 0))
            if o.shape[2] == 3:
                o = o[:, :, ::-1].copy()  # RGB -> BGR
            writer.send(np.ascontiguousarray(o).tobytes())
            n += 1
            if progress and (n % 15 == 0):
                elapsed = time.perf_counter() - t_start
                fps = n / elapsed if elapsed else 0
                progress(n, fps)
        writer.close()
        cap.release()

    with open(tmp_path, "rb") as f:
        data = f.read()
    os.remove(tmp_path)
    meta = {
        "src": src_path, "in": f"{src_w}x{src_h}", "out": f"{out_w}x{out_h}",
        "mode": mode, "mode_requested": requested_mode, "downshifted": downshifted,
        "scale": scale, "frames": n, "fps": round(src_fps, 2),
        "bytes": len(data), "gpu": GPU["name"],
    }
    return data, meta


class Handler(BaseHTTPRequestHandler):
    server_version = "VSRProxy/0.1"

    def log_message(self, format, *args):
        # minimal request log to stderr for verification
        sys.stderr.write("REQ " + (format % args) + "\n")

    def _send(self, code, body, ctype="text/plain; charset=utf-8", extra=None):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        if extra:
            for k, v in extra.items():
                self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        try:
            if u.path == "/health":
                import json
                body = {"ok": GPU["ready"], "gpu": GPU.get("name"),
                        "free_mb": GPU.get("free_mb"), "total_mb": GPU.get("total_mb"),
                        "err": GPU.get("err")}
                self._send(200, json.dumps(body), "application/json")
            elif u.path == "/":
                self._send(200, INDEX_HTML, "text/html; charset=utf-8")
            elif u.path == "/play":
                # Serve a pre-generated upscaled clip directly (browser playback proof).
                p = q.get("f", ["/tmp/vsr_test.mp4"])[0]
                if not os.path.isfile(p):
                    self._send(404, f"no such file: {p}")
                    return
                with open(p, "rb") as fh:
                    data = fh.read()
                self._send(200, data, "video/mp4", extra={
                    "Content-Disposition": 'inline; filename="vsr.mp4"',
                    "Accept-Ranges": "bytes",
                })
            elif u.path == "/upscale":
                if not GPU["ready"]:
                    self._send(503, "GPU not ready: %s" % GPU.get("err"))
                    return
                src = q.get("src", [None])[0]
                mode = int(q.get("mode", ["2"])[0])
                scale = float(q.get("scale", ["1.5"])[0])
                strict = q.get("strict", ["0"])[0] in ("1", "true")
                if not src or not os.path.isfile(src):
                    self._send(400, "missing/invalid ?src= (must be an existing file path)")
                    return
                t0 = time.perf_counter()
                data, meta = upscale_clip(src, mode, scale, GPU["gpu_index"], strict=strict)
                dt = time.perf_counter() - t0
                import json
                self._send(200, data, "video/mp4", extra={
                    "X-VSR-Meta": json.dumps(meta),
                    "X-VSR-Elapsed-S": f"{dt:.2f}",
                })
            else:
                self._send(404, "not found")
        except InsufficientVRAM as e:
            # Graceful, actionable — NOT a crash. Tell the browser what to do.
            self._send(503, str(e))
        except ContextWedged as e:
            self._send(503, str(e))
        except Exception as e:
            import traceback
            traceback.print_exc()
            self._send(500, f"error: {e}")


INDEX_HTML = """<!doctype html><html><head><meta charset=utf-8>
<title>RTX VSR Proxy</title>
<style>
 body{font-family:system-ui,sans-serif;background:#0f1115;color:#e6e6e6;margin:0;padding:24px}
 h1{font-size:20px;margin:0 0 4px} .sub{color:#8a8f98;font-size:13px;margin-bottom:20px}
 .card{background:#171a21;border:1px solid #262b35;border-radius:10px;padding:16px;margin-bottom:16px;max-width:900px}
 label{font-size:13px;color:#aab;display:block;margin:8px 0 4px}
 select,input[type=number]{background:#0f1115;border:1px solid #2c3340;color:#e6e6e6;
   padding:6px 8px;border-radius:6px;font-size:14px}
 button{background:#7c5cff;border:0;color:#fff;padding:9px 16px;border-radius:7px;
   font-size:14px;cursor:pointer;margin-top:12px}
 button:disabled{opacity:.5;cursor:default}
 video{width:100%;max-width:880px;border-radius:8px;background:#000;margin-top:14px}
 .meta{font-size:12px;color:#8a8f98;margin-top:10px;font-family:ui-monospace,monospace;white-space:pre-wrap}
 .ok{color:#5ad67a}.err{color:#ff6b6b}
</style></head><body>
<h1>RTX VSR Proxy <span id="gpu" class="sub"></span></h1>
<div class="sub">Route-B core: decode → NVIDIA VSR (GPU) → H.264 encode → your browser.</div>
<div class="card">
 <label>Source clip</label>
 <select id="src"></select>
 <div style="display:flex;gap:18px;flex-wrap:wrap">
   <div><label>VSR quality</label>
     <select id="mode"><option value=1>LOW</option><option value=2 selected>MEDIUM</option>
       <option value=3>HIGH</option><option value=4>ULTRA</option></select></div>
   <div><label>Scale (×)</label>
     <input type=number id=scale value=1.5 step=0.25 min=1 max=2 style="width:90px"></div>
 </div>
 <button id="go">Upscale &amp; play</button>
 <div class="meta" id="status"></div>
 <video id="v" controls style="display:none"></video>
</div>
<script>
const CLIPS = __CLIPS__;
const sel = document.getElementById('src');
CLIPS.forEach(c=>{const o=document.createElement('option');o.value=c;o.textContent=c;sel.appendChild(o);});
fetch('/health').then(r=>r.json()).then(h=>{
  const g=document.getElementById('gpu');
  if(h.ok){g.textContent='· '+h.gpu+' · '+(h.free_mb/1024).toFixed(1)+' GB free';g.className='sub ok';}
  else{g.textContent='· GPU ERROR: '+(h.err||'?');g.className='sub err';}
});
document.getElementById('go').onclick=async()=>{
  const src=sel.value, mode=document.getElementById('mode').value, scale=document.getElementById('scale').value;
  const v=document.getElementById('v'), st=document.getElementById('status'), btn=document.getElementById('go');
  btn.disabled=true; v.style.display='none';
  st.textContent='Upscaling '+src+' … (this runs the full pipeline)';
  const t0=performance.now();
  try{
    const r=await fetch('/upscale?src='+encodeURIComponent(src)+'&mode='+mode+'&scale='+scale);
    if(!r.ok){throw new Error((await r.text()));}
    const blob=await r.blob();
    v.src=URL.createObjectURL(blob); v.style.display='block'; v.play().catch(()=>{});
    const ms=((performance.now()-t0)/1000).toFixed(1);
    const m=JSON.parse(r.headers.get('X-VSR-Meta')||'{}');
    const MN={1:'LOW',2:'MEDIUM',3:'HIGH',4:'ULTRA'};
    const q=m.downshifted?('⚠ '+MN[m.mode_requested]+' requested → '+MN[m.mode]+' used (VRAM) · '):'';
    st.textContent='done in '+ms+'s · '+(blob.size/1048576).toFixed(1)+' MB · '+(m.in||'')+'→'+(m.out||'')+' · '+q+MN[m.mode];
  }catch(e){st.textContent='error: '+e;st.className='meta err';}
  btn.disabled=false;
};
</script></body></html>"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8730)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--gpu", type=int, default=int(os.environ.get("VSR_GPU", "0")))
    ap.add_argument("--clips-dir", default=os.environ.get("VSR_CLIPS_DIR", os.path.expanduser("~/VSR/spike")))
    args = ap.parse_args()

    _gpu_setup(args.gpu)

    # build clip list for the demo page
    clips = []
    d = args.clips_dir
    if os.path.isdir(d):
        for fn in sorted(os.listdir(d)):
            if fn.lower().endswith((".mp4", ".webm", ".mkv", ".mov")):
                p = os.path.join(d, fn)
                if os.path.isfile(p):
                    clips.append(p)

    global INDEX_HTML
    import json as _json
    INDEX_HTML = INDEX_HTML.replace("__CLIPS__", _json.dumps(clips))

    srv = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"# VSR proxy on http://{args.host}:{args.port}/")
    print(f"# GPU {args.gpu}: {GPU['name']}  ({GPU['free_mb']/1024:.2f} GB free)")
    print(f"# clips dir: {d} -> {clips}")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
