# RTX Video Super Resolution on Linux — Design Doc

Status: draft v1 (2026-08-23)
Hardware target: 2× RTX 4090, driver 595.84, CUDA 13.2 (verified working host)

## 1. Goal

Bring NVIDIA RTX Video Super Resolution (VSR) to Linux so that low-resolution
video (360p–1080p streams, local files) is AI-upscaled in real time — ideally
inside the browser, like Windows Chrome does today. Open source on GitHub with
a structure that invites collaboration.

## 2. How it works on Windows (the reference implementation)

Key finding: **there is no user-space filter, model, or MFT.** Chromium's
entire integration (commit `46f000a1de9e77a`, "Enable NVIDIA video processor
super resolution on Windows") is a ~40-line hook in the swap-chain presenter:

- Chrome scales video to screen via `ID3D11VideoContext::VideoProcessorBlt`.
- Before the blt it calls `VideoProcessorSetStreamExtension` with NVIDIA's
  private GUID `{d43ce1b3-1f4b-48ac-baee-c3c25375e6f7}` ("PPE" = post-process
  extension), payload `{version=1, method=2 /*SuperResolution*/, enable}`.
- The Windows driver (`nvoglv64.dll`) intercepts the GUID and runs the VSR
  TensorRT engine **inside the driver**, on the video texture, during the blt.
- Enablement heuristics (from Chromium source + NVIDIA FAQ): AC power only,
  sub-display-resolution video, DRM/secure-content excluded, quality level set
  in nvidia-smi/control panel registry keys read by the driver.

Consequences:
- Zero-copy, zero user-space cost — this is why it "just works" in Chrome/Edge
  and VLC on Windows with no plugin.
- The model binary lives only in the Windows driver. It is **not** present in
  the Linux driver package.

## 3. Why it's missing on Linux

1. No D3D11 video processor exists natively; DXVK implements only rudimentary
   `VideoProcessorBlt` over Vulkan Video and has no path for NVIDIA's private
   stream-extension GUID (and nothing to forward it *to*).
2. The Linux driver does not ship or expose the VSR model.
3. Chromium's Linux video path has no equivalent "video processor" hook:
   decode → `GpuVideoFrame` (DMA-BUF) → `viz::SharedImage` → ANGLE/Vulkan
   compositing. Scaling to screen is an ordinary texture draw with no vendor
   extension point.

Therefore the Linux solution must perform, in user space, what the Windows
driver does internally: take decoded frames, run the VSR model on the GPU,
hand the result back to the compositor.

## 4. What NVIDIA gives us for free on Linux

**VFX SDK** (Video Effects SDK, successor of MAXINE Video Effects):
- C++ library `nvvfx` + Python bindings (`pip install nvidia-vfx`, bundles the
  libraries in the wheel — no NGC key needed for the wheel).
- The `VideoSuperRes` effect: input/output are GPU buffers (BGRA/RGBA 8-bit,
  or DLPack CUDA arrays via the Python API), modes 0–19:

| Mode | Name            | Note                          |
|------|-----------------|-------------------------------|
| 0    | Bicubic         | baseline, no AI               |
| 1–4  | Low…Ultra       | AI upscale (1–2× typical)     |
| 8–11 | Denoise L…U     | same-res denoising            |
| 12–15| Deblur L…U      | same-res deblurring           |
| 16–19| HighBitrate L…U | AI upscale for clean sources  |

- Linux driver requirement: **570.190+, 580.82+, or 590.44+** → we have 595.84 ✓
- Suggested minimum input: 360p. Output aspect free; keep = input for best quality.
- Caveat (docs): SDK is "designed and optimized for server-side deployment… we
  do not officially support client-side application integration." The NGC
  feature list advertises datacenter GPUs, **but** the `nvvfxsuperres` NGC
  collection documents an explicit RTX 4090 (sm_89) install example, and the
  pip wheel is consumer-friendly. → **Spike #1 verifies this on our hardware.**

Related prior art:
- `gst-nvmaxine` (GitHub, LGPL): GStreamer element wrapping the older VFX SDK,
  effects incl. SuperRes. Last active ~2023 against SDK 0.7.2 — a fork/refresh
  is our GStreamer path.
- `mpv-AnimeJaNai`: custom mpv build doing real-time Real-ESRGAN via TensorRT
  (Windows). Proof that the "player + TRT model" pattern works; different model,
  same architecture idea.
- Intel VP Super Resolution is upstreamed in Chromium (`kIntelVpSuperResolution`)
  — precedent that Google accepts vendor video-enhancement hooks.

### 4a. Hardware support matrix

NVIDIA's documentation names Ampere (RTX 30xx) as the floor, but the model
library bundled in the wheel (`libnvidia-ngx-vsr.so`) embeds CUDA kernels for
**sm_75 (Turing)**, sm_80, sm_86 and sm_89, and every mode — LOW to ULTRA,
denoise, TrueHDR — loads and runs on an RTX 2060 SUPER
(`spike/probe_turing.py`, `spike/results_turing.txt`). So the gate in
`install/check_gpu.py`, which classifies each GPU from `nvidia-smi`, is:

| Tier | Compute cap | Cards | Gate verdict / exit | Behavior |
|------|-------------|-------|---------------------|----------|
| Supported | sm_75+ (Turing, Ampere, Ada) | 20xx and newer (4090 ref) | `supported` / 0 | Normal install & run. On Turing the report adds a note: prefer LOW/MEDIUM for 60 fps or 4K output. |
| Unsupported | < sm_75, no NVIDIA GPU, or driver below min | GTX 10xx and older | `unsupported` / 20 | Refused with a clear message; point at the driver/GPU requirement. |

Driver floor: **≥ 570.190 / 580.82 / 590.44** (verified on 595.84 and
595.104). The check parses `nvidia-smi` for both compute capability and driver
branch, so a too-old driver on an otherwise-fine card is caught at install
time, not at the first OOM or missing-kernel error.

Turing is slower, not riskier: the 2060 SUPER runs MEDIUM 720p→1440p at
4.3 ms/frame and ULTRA 1080p→4K at 31 ms/frame, about a tenth of a 4090. The
players handle that by stepping the mode down when frames arrive late (§5C).

## 5. Architecture options (ranked)

### A. Chromium compositor patch — the real goal (hard)

Linux frame path: `media/gpu` decoder (VA-API/NVDEC or Vulkan Video) →
`GpuVideoFrame` backed by DMA-BUF → `viz::SharedImage` → ANGLE/Vulkan draw in
the UI compositor.

Hook point (proposed): right after decode, before the frame enters viz:

```
decode (NVDEC/cuvid, frames already live in GPU memory as DMA-BUF)
  └─> [PATCH] if flag enabled && frame_res < display_res && !secure_content:
        1. import DMA-BUF fd into CUDA   (cudaImportExternalMemory, OPAQUE_FD —
                                          zero-copy; Chromium already does this
                                          class of import for NVDEC frames)
        2. convert/format to BGRA if needed (NVDEC gives NV12 → small kernel or
           cuvid's own RGB output mode to avoid the swizzle)
        3. nvvfx::VideoSuperRes.run()   (the actual AI model, TensorRT on GPU)
        4. export result buffer back to a DMA-BUF fd
                                          (cuMemCreate + POSIX FD shareable handle)
        5. wrap in a new GpuVideoFrame at the upscaled size
  └─> normal viz/ANGLE compositing (now scaling an already-HR frame)
```

Supporting pieces:
- Feature flag `kLinuxNvidiaVsr` (+ quality level, min/max source res, AC-power
  check mirroring Windows behavior).
- Frame scheduler: VSR runs on a dedicated thread pool; if the model can't keep
  up at the requested fps, drop to a lower quality mode or bypass (never block
  playback). Target budget: <16 ms/frame at 1080p→4K on a 4090.
- Secure-content/DRM frames must be detected and bypassed (same as Windows).

Effort: **3–8 weeks** to a working private build (interop details are the main
risk; every primitive — DMA-BUF↔CUDA import, GpuVideoFrame construction — has
precedent in-tree). Upstreaming: plausible but not guaranteed (vendor-specific
library dependency is the sticking point; Intel's hook is driver-side and
cheaper for Google). Fallback: ship as a distro/fork patch (Brave, ungoogled,
or our own build) — still fully useful.

### B. GStreamer element → VLC & friends (medium, immediate value)

Fork/refresh `gst-nvmaxine` against the current VFX SDK:
`nvdec ! nvvfx-vsr(mode=high, scale=2) ! ...`. Works in VLC, GStreamer-based
apps, and as a CLI pipeline (`gst-launch-1.0`). Also the cleanest *benchmark
harness* for option A. Effort: **~1 week**.

### C. mpv filter — implemented (`mpv/`)

mpv's `vapoursynth` video filter embeds a Python interpreter, which is all
`nvvfx` needs: `mpv/vsr.vpy` is a VapourSynth script that runs the model on
each frame, and `mpv/main.lua` adds and removes it at runtime (`Alt+v`). The
stock Flathub mpv is built with the filter, so there is nothing to compile.

What made it work, in the order the problems appeared:

- **mpv only accepts integer YUV from VapourSynth.** A script that returns the
  model's float RGB is rejected with "Unsupported output format" and mpv
  silently disables the filter. So the script converts on the GPU: YUV planes
  as decoded → RGB float (CUDA kernel, bilinear chroma at the true siting) →
  model → YUV at the output size (second kernel) → download straight into the
  output frame. Checked against zimg around the same model in
  `spike/test_vpy.py`: mean error 0.3 of an 8-bit step, no bias, no shift.
  Integer planes also cut host↔GPU traffic 4–8× compared with float RGB.
- **Output frames inherit the input's colour tags.** mpv copies matrix, range
  and chroma location from its input params onto whatever the filter returns,
  so the output kernel must use the same matrix/range/siting it read from the
  input frame's props.
- **mpv derives timestamps from `_DurationNum/_DurationDen`**, so all frame
  props are copied from the source frame to the new one.
- **Range prop names differ by VapourSynth version.** Up to R73 it is
  `_ColorRange` (1 = limited); newer versions use `_Range` (0 = limited) and
  alias reads of the old name to the new *value*. The script reads whichever
  key is really present.
- **The embedded interpreter has no site-packages of ours.** The script adds
  a venv built for the same Python version (`~/vsr-venv-py313` for the
  Flatpak) with `site.addsitedir`, extends `site.getsitepackages()` so
  cuda-pathfinder finds the pip-shipped CUDA libraries, and moves cupy's
  kernel cache out of the read-only sandboxed `$HOME`. No `PYTHONPATH` or
  `LD_LIBRARY_PATH` is needed.
- **mpv re-evaluates the script on every seek** and VSScript wipes the old
  evaluation's globals. The loaded engine is parked in `sys.modules` as plain
  data (no functions or VS objects from the script), so seeks reuse it.
- **Output size.** Any even size works with nvidia-vfx 0.2.0.0 — fractional
  factors too (720p→1080p, 1080p→1440p) — so the target is "fit the display",
  not a fixed 2×. `display_res` as mpv passes it to the script is a
  placeholder on Wayland until the window is mapped; the Lua script passes the
  real `display-width/height` and re-applies when they change.
- **Overload.** `estimated-vf-fps` reports the nominal rate even when the
  filter is too slow; the reliable signal is the VO's `frame-drop-count`
  rising. The Lua script steps the mode down after three such seconds.

Frames still make one round trip through system memory (decode → VS frame →
GPU → VS frame → VO). Zero-copy would need a native filter on mpv's hwdec
frames; at 7 ms/frame total for 720p→1440p on a 2060 SUPER it has not been
worth it yet.

### D. Not viable (recorded for completeness)

- Wayland/X11 compositor-level hook: compositors never see decoded video
  frames, only presented buffers — no frame access, huge latency if forced.
- Screen-capture + upscale: defeats the purpose (latency, re-encode, DRM).

## 6. Reverse-engineering scope (light, and mostly contingency)

Happy path needs **no RE** — NVIDIA ships the model officially for Linux.
RE only enters if Spike #1 shows the SDK misbehaving on consumer Ada:

1. **Contingency:** extract the VSR TensorRT engine from the Windows driver
   (`nvoglv64.dll`) and load it via TensorRT on Linux. Method: proxy-DLL hook
   on a Windows box to capture what the driver loads/initializes, or carve the
   embedded `.engine` blob (binwalk/strings + TRT plan-file magic). Moderate
   effort; fragile across driver updates; ToS gray zone → keep as fallback,
   document honestly in the repo.
2. **Optional:** mirror Windows enablement heuristics exactly (quality
   selection per resolution/fps) — mostly readable from Chromium source and
   NVIDIA KB; only driver-internal quality tables would need light RE.

## 7. Repository layout & licensing

```
rtx-vsr-linux/
├── docs/DESIGN.md            ← this document
├── spike/                    ← M1: nvidia-vfx pip bench (ms/frame, OOM headroom)
├── lib/vsr-glue/             ← C++ wrapper: CUDA buffer in/out, frame pool, mode API
├── gstreamer/gst-nvvfx/      ← M2: refreshed GStreamer element (fork of gst-nvmaxine)
├── mpv/                      ← M3: vsr.vpy (VapourSynth filter) + main.lua (toggle)
├── app/                      ← browser path: GPU proxy + control window (Rust, §9g)
├── extension/                ← browser path: MSE hook that feeds the app (§9f)
├── chromium/patches/         ← M4: feature flag + post-decode hook + interop
├── benchmarks/               ← ms/frame tables per mode/resolution; A/B screenshots
├── install/                  ← SDK fetch scripts (NGC), driver checks, CI matrix
│   ├── check_gpu.py          ← hardware gate: supported/unsupported
│   ├── install_app.sh        ← builds the app, installs ~/.local/bin/rtx-vsr
│   └── install_mpv.sh        ← venv for mpv's embedded Python + script link
```

Licensing: our code MIT. The VFX SDK + models are NVIDIA-proprietary (NGC
license) — **not redistributable**; the repo ships only glue code plus an
install script that pulls the SDK. Precedent: gst-nvmaxine (LGPL plugin linking
a proprietary lib). README must state this up front for collaborators.

## 8. Milestones & acceptance criteria

| M | What | Acceptance | Est. |
|---|------|-----------|------|
| 1 | Spike: `nvidia-vfx` runs on consumer 4090 | ms/frame table (720p→1440p, 1080p→4K; modes 1–4); fits in free VRAM; no NGC key needed at runtime | **DONE** — see `spike/results_m1.txt` (0.5–3.1 ms/frame, ~0.2–0.6 GB) |
| 1.5 | End-to-end pipeline probe: real decode→VSR→encode faster than real-time | per-stage ms/frame breakdown; production NVDEC+AI+NVENC estimate vs 30/60 fps budget; A/B visual proves genuine upscaling | **DONE** — see `spike/results_m1_5.txt` (GPU-resident AI path 0.63 ms/f; full pipeline 6.6–11.6 ms/f = 2.5–5× headroom @30fps) |
| 2 | GStreamer element + `gst-launch` demo on a real file/stream | 60 fps sustained, 1080p→4K, mode ≥ MEDIUM | ~1 wk |
| 3 | mpv filter with keybind toggle | watch YouTube-quality stream / local file upscaled live | **DONE** — `mpv/`, §5C. YouTube 480p AV1 → 1440p through yt-dlp; 1080p60 → 1440p MEDIUM at 86 fps on a 2060 SUPER, zero dropped frames in timed playback |
| 4 | Chromium patch (private build first) | 720p YouTube on 4K display visibly VSR-upscaled, <16 ms/frame budget met, DRM bypass works | 3–8 wks |
| 5 | Upstream / distro patch + CI | PR to chromium or a maintained fork; CI matrix (driver versions, VA-API vs Vulkan Video) | open-ended |

## 9. Risks

- **Consumer-GPU support of the SDK** — *de-risked (M1, 2026-08-23)*: pip
  wheel `nvidia-vfx` 0.1.0.1 runs on consumer RTX 4090 / driver 595.84 with
  no NGC key; see `spike/results_m1.txt`. Remaining caveat is the official
  "server-side only" support statement — treat SDK updates as unpinned.
- **VRAM headroom** — *measured (M1)*: engine + buffers cost ~0.2–0.9 GB extra
  depending on mode/resolution. On this host both GPUs run LM Studio at
  ~22–24 GB/24 GB, so VSR shares VRAM with the local LLM **by design**: the
  proxy auto-downshifts quality to fit live free VRAM (see §9b) instead of
  requiring the model to be unloaded.
- **Chromium interop risk** (DMA-BUF↔CUDA export direction is the unproven
  leg) — de-risk with a standalone prototype in M2 before touching Chromium.
- **Model drift:** NVIDIA ships model updates inside driver/SDK releases; pin
  SDK versions in CI and re-benchmark on each update.
- **Upstream acceptance** uncertain → plan B is fork/distro patches, which is
  still a complete user-facing product.

### 9a. Pitfall (hit & fixed 2026-08-23): nvvfx DLPack outputs are effect-owned views

`cp.from_dlpack(sr.run(x).image)` looks innocent but is a **use-after-free**:
`sr.run(x)` returns a result object whose `.image` DLPack capsule references GPU
memory *owned by that result/effect*. If you don't hold a reference to the
result, Python frees it as soon as `from_dlpack()` returns, leaving a dangling
view. The corruption is **sticky** — the CUDA context poisons and every later
request in the process fails with `NvVFXError: Failed to allocate NvCVImage: an
illegal memory access was encountered` (surfacing at the *next* effect's
`set_output_image`, which is why it looked like a fresh allocation failure).

Correct pattern (used in `proxy/vsr_proxy.py` and `spike/check_correctness.py`):
```python
res = sr.run(x)                      # hold the result alive
out = cp.array(cp.from_dlpack(res.image))  # deep-copy into independent memory
del res, x                           # now safe to release
```
Also serialize all GPU work with a lock (`GPU_LOCK`) — the browser fires many
segment requests concurrently and they must not share the effect/context at once.
A corrupted context only clears on process restart (there is no in-process reset).

### 9b. VRAM-aware auto-downshift (added 2026-08-23)

This host runs LM Studio on the same GPU as VSR, so "not enough VRAM" is the
*normal* state, not an error. The proxy therefore never hard-fails on a mode
that doesn't fit — it **cascades down** to the highest quality that does.

- `est_vram_mb(mode, out_w, out_h)` — per-mode estimate from M1 measured deltas
  (LOW ~0.2 GB … ULTRA ~0.9 GB at 4K) scaled by output megapixels above the
  reference, +30% safety factor +150 MB process headroom.
- `choose_mode(requested, free_mb, out_w, out_h)` — walks modes from `requested`
  down to LOW, returns the first whose estimate ≤ live free VRAM (via
  `cupy memGetInfo`), or `None` if even LOW won't fit.
- On a downshift the proxy logs `# downshift: ULTRA -> MEDIUM (free … MB)` and
  tags the response meta with `mode_requested`, `mode`, `downshifted=true`; the
  demo page shows "⚠ ULTRA requested → MEDIUM used (VRAM)".
- `?strict=1` opts out: exact mode or a clean **503** with an actionable message
  ("free some VRAM / lower the scale"). Default is auto-downshift.

Verified end-to-end (`spike/test_downshift.py`, forced free-VRAM values):
free 5000 MB → MEDIUM as requested; free 500 MB → downshifted to LOW (valid
output); free 300 MB → 503 "even LOW needs ~410 MB". A live ULTRA@2160p request
that previously OOM-faulted and bricked the context now returns valid 4K H.264
when VRAM allows, or degrades when it doesn't — never a poisoned context.

### 9c. Resolution policy (added 2026-08-23)

VSR earns its keep on 360p–1080p sources; a stream already at/above display
resolution needs no AI help and would only spend VRAM the local LLM wants. The
host display is 2560×1440, so the policy (CLI `--max-out`, default **1440**) is:

- **Cap output height** at `max_out` px. A requested scale that overshoots is
  clamped (`plan_output` returns an effective scale), so 720p@3× → 2160p request
  becomes a 1440p output, never more.
- **Pass through** any source whose height is already ≥ `max_out`: served
  verbatim with zero GPU work and zero VRAM (`passthrough_clip`), tagged
  `passthrough=true` in the meta.

Verified end-to-end (`spike/test_policy.py`): 720p@1.5→1080p VSR; 720p@3.0 and
1080p@1.5 both clamped to 1440p output; a 1440p source passed through in ~0 ms
with no VSR run. The demo page shows "pass-through · no VSR" for those.

### 9d. YouTube Server ABR broke server-side fetching (found 2026-08-23) — pivot to browser-as-fetcher

The original plan was "extension rewrites the `googlevideo.com` segment URL →
proxy re-fetches it". **That no longer works.** As of this date the watch page's
`ytInitialPlayerResponse.streamingData` contains:

- **No per-format `url` fields** — each adaptive format only has metadata
  (itag, width/height, mimeType, contentLength).
- A single top-level **`serverAbrStreamingUrl`** (`sabr=1`) — YouTube's new
  "Server ABR" protocol where the *server* picks and serves segments.

That URL returns **HTTP 403 Forbidden to any server-side GET**, even with the
full session cookie jar (YSC, __Secure-YNID, …) and `Range` headers. The stream
is bound to the browser's client context in a way a local process cannot
replicate. Server-side workarounds were also checked and failed: Android-app
UA watch page (`ytInitialPlayerResponse = null` / gated), Innertube `/player`
API (400 Bad Request). Conclusion: **a server cannot fetch YouTube segments.**

**Architecture pivot:** the browser is the only authorized fetcher — so it stays
the fetcher. The proxy becomes a pure **transform service**:

- `POST /transform?mode=&scale=&strict=` — body = raw segment bytes (fMP4/CMAF).
  The extension (or any client) fetches the segment with its own session, POSTs
  the bytes to localhost, and receives VSR-processed H.264 back (or the original
  bytes unchanged when pass-through applies: audio-only or ≥ cap). Same policy
  stack as `/upscale`: resolution cap + VRAM auto-downshift + graceful 503.

This is *more* robust than URL rewriting: it survives YouTube protocol changes
(the browser keeps fetching natively), works per-site via the extension, and the
proxy never needs cookies or signature handling. Verified with real bytes:
360p segment → 1280×720 H.264 High in 1.67 s (~54 fps); 1440p → pass-through 0 ms.

### 9e. Codec coverage — every YouTube stream type, not just MP4 (found 2026-08-23)

YouTube serves three container/codec combos: **H.264/MP4** (most non-Premium),
**VP9/WebM**, and **AV1** (newer + all 4K). The first decoder (OpenCV `cv2`)
handled H.264 and VP9 but has **no AV1 decoder** — an AV1 segment failed with
`Could not find codec`. Fix: decode via **PyAV** (FFmpeg, pip wheel, no system
ffmpeg), which decodes all of them; cv2 is kept as a fallback if PyAV can't open
a file. The encoder side was never a problem — the bundled ffmpeg emits H.264
High regardless of input codec.

Verified through the **live `/transform`** endpoint (MEDIUM, 2×, 30 fps):

| Input | Codec / container | Result | Speed |
|---|---|---|---|
| `seg_360p.mp4` | H.264 / MP4 | 1280×720 H.264 High ✓ | ~47 fps |
| `test_vp9.webm` | VP9 / WebM | 1280×720 H.264 High ✓ | ~50 fps |
| `test_av1.mp4` | **AV1** / MP4 | 1280×720 H.264 High ✓ (was failing) | ~51 fps |

So the "non-MP4 YouTube stream" concern is fully covered: VP9 and AV1 both
upscale, output is always H.264 (what browsers play back best), and sources
already ≥ 1440p still pass through untouched. Note: a static imageio-ffmpeg
build has no `*_cuvid` NVDEC decoders, so decode runs on CPU via PyAV — fine at
these rates, but the natural next optimization is `av1_cuvid`/`h264_cuvid`
hardware decode on the 4090.

### 9f. Making the browser path MSE-correct (2026-10-10)

§9d decided *what* the proxy is; this is what it took to make a player accept
its output. Implemented in `proxy/vsr_segment.py` and `extension/`, checked by
`spike/test_segment_mse.py` and `spike/test_extension.py` (a real headless
Chromium playing a DASH stream through the extension).

**The proxy side**

- A media segment (`moof+mdat`) is not decodable without the stream's init
  segment, and players append the two separately. The extension keeps the
  init segment per SourceBuffer and sends `init + fragment`.
- The output's `tfdt` must be the fragment's real decode time. FFmpeg's MP4
  muxer rebases a fragmented output to zero (`empty_moov` implies
  `avoid_negative_ts=make_zero`), which would stack every segment at t=0.
  `movflags=+frag_discont` with `avoid_negative_ts=disabled` writes absolute
  times.
- Chromium's MSE parser applies a single-entry edit list (the usual B-frame
  delay compensation), exactly as FFmpeg's demuxer does by default. So the
  decoded frame PTS is already the timeline position the browser would have
  used; copying it to the output frame is correct. Ignoring the edit list
  shifts the whole stream by the B-frame delay — measured as a 67 ms offset
  between the control run and the transformed run before this was fixed.
- No B-frames and a fixed GOP in the output: PTS == DTS, every fragment
  starts on a keyframe.
- The response starts with the output's own init segment
  (`X-VSR-Init-Length`). It is byte-identical from call to call, so the
  extension appends it once and again only if it changes. `204` means
  "leave the original alone" (audio, source ≥ cap).

**The extension side**

- A content script in the default isolated world cannot patch the page's
  `SourceBuffer.prototype` — and `SourceBuffer` has no `type` property to tell
  video from audio. The hook runs in the **MAIN world** and learns the mime
  type by wrapping `MediaSource.addSourceBuffer` / `changeType`.
- To be in place before the player and without an async config read, the
  service worker *registers* the scripts (`chrome.scripting`) only for the
  enabled sites. Presence of the hook is the per-site switch.
- The page calls `appendBuffer` and expects `updating === true` and an
  `updateend`. While a fragment is at the proxy the hook reports
  `updating`, refuses `remove()` and setters like `timestampOffset` with
  `InvalidStateError`, and fires the events itself when an append produced no
  bytes yet (data may arrive in arbitrary pieces; it is collected into whole
  top-level boxes). `abort()` cancels the in-flight fragment.
- Requests to the proxy go page hook → isolated content script
  (`postMessage`, transferable buffers) → service worker (`fetch`). A fetch
  from the page itself would be subject to the site's CSP, CORS and the
  local-network permission prompt. Extension messages are JSON, hence base64.
- Sites that prefer VP9/AV1 are steered to H.264 by reporting those codecs as
  unsupported (`isTypeSupported`, `canPlayType`, `mediaCapabilities`). On
  YouTube this yields avc1 up to 1080p.
- Failure is always "append the original bytes"; after three failed
  transforms in a row the hook stops trying until the page reloads. If the
  proxy's init segment is in effect, the page's own init segment is
  re-appended first.

**State** (as of the Python proxy; §9g replaces the proxy side). The local test passes (whole segments, 64 kB pieces, with audio):
640×360 in, upscaled video out, buffered range identical to the unmodified
run, no dropped frames. On YouTube (headless, Big Buck Bunny): avc1 720p60
served, decoded as 1920×1080, 8 segments / 2474 frames transformed, 41 s
buffered continuously, no errors — but headless Chromium pauses YouTube with
or without the extension, so sustained playback, quality switches, ads and
live streams are unverified. Cost is CPU-bound: 720p60 → 1080p at 11.4
ms/frame end to end with x264, 1.4× real time on the 2060 SUPER machine.
NVENC is not in the PyAV or imageio-ffmpeg builds; it needs a system FFmpeg
or a custom PyAV.

### 9g. The app: the proxy as one all-GPU program (2026-10-11)

§9d–§9f left the browser path as a Python script plus an extension to load by
hand, with decode and encode on the CPU. `app/` replaces the script (which is
gone, with its tests) by one Rust program that is the proxy and a window to
control it. The protocol towards the extension is §9f's, minus the knobs:
quality and target size are the app's settings now, and the extension only
reports which screen the page is on.

**The pipeline** (`app/src/engine/`, `app/src/gpu/`), one fragment at a time on
a single thread that owns the CUDA context:

```
init + moof/mdat ─ demux (mp4.rs) ─ Annex B ─ NVDEC (cuvid parser + decoder)
   NV12 in CUDA memory ─ NvCVImage_TransferFromYUV ─ BGRA ─ VideoSuperRes
   BGRA ─ NvCVImage_TransferToYUV ─ NV12 ─ NVENC ─ length-prefixed NALs ─ mux
```

Nothing is copied to system memory except the compressed bytes. On the
2060 SUPER, 720p → 1440p MEDIUM takes 7.5 ms/frame in total (the model alone
is 4.3), where the Python proxy needed 11.4 ms/frame for 720p → 1080p.

What it took, in the order the questions came up:

- **No Python is needed to run the model.** The `nvidia-vfx` wheel is the VFX
  SDK's C libraries plus a thin binding; `NvVFX_*` and `NvCVImage_*` are
  exported and match the SDK's public headers (pixel-format enum included:
  `NVCV_YUV420` is 10). The libraries' RPATHs do not cover each other, so they
  are opened by full path, dependencies first, with `RTLD_GLOBAL` — what
  NVIDIA's `_lib_loader.py` does. VSR needs only a dozen of them (175 MB of
  the wheel's 580); the app downloads the wheel from pypi.nvidia.com itself
  (pinned version, SHA-256 checked) or uses a copy that pip installed.
- **`VideoSuperRes` takes 8-bit BGRA or RGBA, chunky, in CUDA memory**, and
  nothing else: planar float (what the Python binding exposes) is refused
  with `NVCV_ERR_PIXELFORMAT`. That is the convenient case — it is what NV12
  converts to in one `NvCVImage_Transfer`.
- **`NvVFX_SetImage` keeps the pointer to the `NvCVImage` descriptor**, not a
  copy. A descriptor that moves afterwards (a struct returned by value) makes
  `NvVFX_Run` fail with a pixel-format error that points nowhere near the
  cause. The descriptors are boxed.
- **A cuvid parser cannot be reset**, so each fragment gets a new one (it is
  only a bitstream parser); the decoder is kept while the format stays the
  same. Timestamps go in with each access unit and come back with each
  picture in presentation order; a fragment whose pictures do not all come
  back is refused rather than appended with holes.
- **NVENC**: SDK 13.0 structures (driver 570+, the VFX floor), bindings
  generated from FFmpeg's `nv-codec-headers` and loaded with `dlopen`, so a
  missing driver is a message in the window, not a failure to start. No
  B-frames, IDR at the start of every fragment and every second after,
  constant quality with a ceiling of 0.09 bit per pixel per frame (10 Mbit/s
  at 1440p30).
- **Colour.** A stream that does not say which matrix it uses is shown by
  Chromium as BT.601 below 720 lines and BT.709 from there up. Upscaling moves
  a 360p stream across that line, so the output always carries an explicit
  description: the source's, or what Chromium would have assumed for the
  source's size. Both conversions use the same matrix and range.
  (`spike/test_app_proxy.py` measures the difference from the source: 1.1 of
  an 8-bit step, against 3.5 with the wrong matrix.)
- **The target size comes from the displays.** The picker ends at the largest
  display; each request carries the page's `screen` size, the app finds the
  display that matches and never upscales past it (fit inside
  `min(chosen, that display)`, at most the model's 4×).
- **Two automatic step-downs**: the VRAM estimate of §9b, checked when a model
  is loaded, and speed, measured on the frames themselves (see *Keeping up
  with playback* below).
- **Upscaled video is several times the bytes** the player budgeted for, and
  Chromium's SourceBuffer holds 150 MB. When the real append hits
  `QuotaExceededError` the hook keeps `updating` true and retries each second,
  which paces the player the way a slow network would.
- **GPU memory is released after 45 s without work** (decoder, model,
  encoder); the CUDA context stays. Nothing is created until the first
  fragment arrives, so the app idles at zero VRAM.
- **Only extensions may use the endpoint.** A request with a web page's
  `Origin`, or a foreign `Host`, gets 403: a site must not be able to drive
  the GPU of whoever visits it.

**Keeping up with playback** (later on 2026-10-11, after video kept freezing
in real use). The first version answered a fragment when all of it was done
and judged its speed fragment by fragment. With YouTube's 5 s fragments that
is seconds of nothing at every start and seek, and a mode that is too slow is
only left after the player has run dry twice. `spike/test_playback.py` plays
such streams through the extension the way a player does (a few seconds
buffered ahead, a seek into video that is not there yet) and measures it.
What changed:

- **A fragment comes back in parts.** `POST /transform?parts=1` answers as
  soon as the first 0.4 s of video are encoded, and names the fragment in
  `X-VSR-Next`; `POST /transform/next?job=` returns what has been finished
  since (a part is cut every 250 ms of work), `POST /transform/cancel?job=`
  says the rest is not wanted. The hook appends each part to the real buffer
  as it comes, behind the page's back: the page still sees one append,
  `updating` throughout, and one `updateend` when the last part is in.
  `abort()` (a seek) cancels the fragment in the app too, so the GPU is not
  kept busy with frames nobody will see.
- **The player says where it is**, with the fragment and again with each
  request for a part: `lead`, the seconds buffered in front of the playhead
  (not said while paused), `behind`, the seconds of this fragment nobody is
  waiting to see (the playhead is past them after a seek into its middle),
  and `rate`. Frames behind the playhead are made with LOW, and so is
  everything while less than 1 s is buffered, until there are 2 s again:
  a player about to run dry needs frames now more than it needs them good.
- **The mode is the best one that is faster than the video plays, measured
  while it runs.** Every ten frames, what a frame took is compared with what
  it may take (its duration, less 15 %, divided by the playback rate). Too
  slow and the next frame is made one mode down, in the middle of the
  fragment. A miss by less than a quarter has to happen twice, and that mode
  gets another chance two minutes later: something else may have held the
  GPU up. The measurements are kept per input size, output size and mode for
  as long as the app runs, so the next video starts in the mode that fits.
- **One model is loaded at a time.** Models that are loaded together do not
  give their memory back when they are dropped (the SDK's doing); loaded one
  after the other they do. Swapping takes about a tenth of a second, which
  the hurry rule's two thresholds keep from happening frame by frame.
- **The encoder works while the model does.** NVENC is hardware of its own.
  It has two input and two output buffers now: `submit` starts a picture and
  returns, the model's work on the next picture is queued, and only then is
  the encoded one collected.

Measured in a window on the 165 Hz display, ULTRA asked for, output
2560×1440 (first frame / a seek into unbuffered video / stood still during
10 s of playback):

| Stream | Before | After |
|--------|--------|-------|
| 1080p30 | 4.9 s / 6.0 s / 0 s, ULTRA | 0.5 s / 0.9 s / 0 s, ULTRA |
| 720p60 | 5.0 s / 6.8 s / 0 s, ULTRA | 0.4 s / 1.3 s / 0 s, ULTRA |
| 1080p60 | 9.4 s / 7.5 s / 3.0 s, HIGH | 0.4 s / 1.3 s / 0 s, MEDIUM |

Over a minute of playback with 30 s buffered ahead nothing stood still, the
app's GPU memory stayed where it was (0.6–1 GB), and frames reached the
screen late (by more than half a frame) as often as with upscaling switched
off: 2–4 % of them, which is this browser on this display. The GPU is
92–100 % busy while it works on ULTRA.

**Getting the extension into the browser without the user doing it.** A
running browser offers nothing: remote debugging is off (and refused for the
default profile since Chrome 136), policy and external-extension directories
need root or the Web Store, and intercepting HTTPS would mean a root
certificate. What is left is the start: the app writes a copy of the
browser's `.desktop` file into `~/.local/share/applications` with
`--load-extension=<dir> --disable-features=DisableLoadExtensionCommandLineSwitch`
on every `Exec` line, and the extension into a directory that browser can
read (inside `~/.var/app/<id>` for a Flatpak). The manifest carries a public
key, so the extension has the same ID wherever it is loaded from. The
override is marked, refreshed at each start of the app and removed by the
same switch; a launcher the user wrote is left alone.
`spike/test_extension.py` starts the browser with exactly the override's
command line.

**The indicator** is shown while upscaled frames are playing, and only then.
Whether a video is showing upscaled frames is read from the decoder, not
assumed: the hook remembers the output sizes the app returned for a stream,
and a `<video>` playing that MediaSource whose `videoWidth × videoHeight` is
one of them is showing them. Size alone is not enough where an original can
be as large as the output (1080p video on a 1080p-class display is left
alone, 720p is upscaled to the same 1920×1080, and a player moves between
the two by itself). So the hook also reads each fragment's place on the
timeline from its `moof` (`tfdt` and the `trun` sample times, with the
track's timescale, edit list and `trex` defaults from the init segment, plus
the buffer's `timestampOffset`) and keeps, per SourceBuffer, the stretches
that upscaled video was appended for; a later append, upscaled or not,
replaces what was there for its stretch, as it does in the buffer. The
video's `currentTime` has to be in one of them. Notes are taken when the
real append happens, not when the app answers, so an aborted append leaves
none. If a fragment's times cannot be read (or the buffer is in `sequence`
mode), that stream falls back to sizes alone, forgetting a size as soon as
an original of that size is appended; a stream the hook stops intercepting
forgets everything.

"Playing" means not paused, not ended and `readyState` above
HAVE_CURRENT_DATA, which also covers buffering stalls and seeks. The
video's own events (caught on `document` in the capture phase, since they
do not bubble) make the mark react at once; a 250 ms timer sees the rest.
The app's settings are asked for every second, and no answer counts as
"not upscaling": the mark goes when the app does, although upscaled frames
may still be in the buffer.

The mark is an `<img>` of `extension/icons/indicator.png` (a web-accessible
resource, which a page's `img-src` does not apply to; styled through style
properties, never a style attribute) placed in the picture's top right corner
inside the video's offset parent, which keeps it under the player's own
controls and inside whatever goes fullscreen. It stays in the page once it
has been shown and fades between 70 % and 0 opacity over 250 ms with a CSS
transition; `visibility` is part of the same transition, so the element is
hidden outright once it has faded out. A mark whose video has left the page
is removed.

**The window** is an iced daemon with one window, software-rendered
(tiny-skia): a settings panel does not need a second GPU context. A second
launch opens a new window instead of trying to raise the old one, which
Wayland does not allow. Its switches are a widget of the app's own
(`app/src/toggle.rs`), because iced's toggler jumps: the knob slides and
the colours blend over 180 ms, driven by the redraw events of the frames in
between, whoever changed the value (a click, the tray's menu). At rest it
is drawn from iced's own toggler style.

**The tray icon** is a StatusNotifierItem (the `ksni` crate, pure Rust over
D-Bus, so no GTK or libappindicator to link). While the desktop has a
StatusNotifierWatcher, closing the window only closes the window; without
one the icon would be nowhere, so closing quits as before, and if the watcher
disappears while the app sits in the tray the window is opened again. The
icon's thread and later launches reach the window's thread through one
channel that is an iced subscription (`ui::request`), not by being polled.
"Start minimized" starts the app at login with the icon only. A login can
get to the app before the desktop's tray exists, so it waits ten seconds for
one and only then falls back to what it did before there was an icon: a
minimised window.

Open: long sessions on YouTube in a normal window (quality switches, ads,
live) — `test_playback.py` plays a local stream, not YouTube's player; VP9
through NVDEC (Turing decodes it) so that steering to H.264 becomes optional.

## 10. References

- Chromium commit 46f000a (NVIDIA VP super resolution, Windows) — the GUID + hook
- docs.nvidia.com/maxine/vfx — VFX SDK guide (VSR modes, Linux driver reqs)
- catalog.ngc.nvidia.com/.../nvvfxsuperres — feature install incl. RTX 4090 example
- pypi.org/project/nvidia-vfx — Python bindings, DLPack API
- github.com/voidmainvoid95/gst-nvmaxine — GStreamer prior art (LGPL)
- github.com/chromium/chromium — `kIntelVpSuperResolution` precedent
- NVIDIA forums: "Will the Linux driver support RTX Video?" (334934) — demand signal
