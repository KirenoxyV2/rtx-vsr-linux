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

### 4a. Hardware support matrix (what we bless vs. what we warn about)

Official VFX support starts at **Ampere (RTX 30xx, sm_8x)**. We target the
**RTX 4090 (sm_89)** as the reference and gate every entry point on
`install/check_gpu.py`, which classifies each GPU from `nvidia-smi`:

| Tier | Compute cap | Cards | Gate verdict / exit | Behavior |
|------|-------------|-------|---------------------|----------|
| Supported | sm_8x+ (Ampere, Ada) | 30xx, 40xx (4090 ref) | `supported` / 0 | Normal install & run. |
| Experimental | sm_75 (Turing) | 20xx (e.g. 2060) | `experimental` / 10 | Runs, but thermally constrained — sustained VSR load gets these hot. Requires explicit opt-in flag before proceeding. |
| Unsupported | < sm_75, no NVIDIA GPU, or driver below min | GTX 10xx and older | `unsupported` / 20 | Refused with a clear message; point at the driver/GPU requirement. |

Driver floor: **≥ 570.190 / 580.82 / 590.44** (verified host runs 595.84). The
check parses `nvidia-smi` for both compute capability and driver branch, so a
too-old driver on an otherwise-fine card is caught at install time, not at the
first OOM or missing-kernel error.

Rationale: we would rather fail fast with an honest "experimental / thermal
risk" notice than let a Turing card cook itself under a sustained 4K upscale
loop and have that blamed on the project. The gate is the single source of
truth for "is this machine allowed in."

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

### C. mpv filter (medium)

mpv on Linux with `--hwdec=nvidia` already keeps frames in CUDA memory; add a
filter that runs nvvfx between decode and the GL renderer, toggled by keybind
(mirrors the Windows `d3d11vpp=scale=2:scaling-mode=nvidia` workflow). Effort:
**1–2 weeks**.

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
├── mpv/vsr-mpv/              ← M3: mpv filter using vsr-glue
├── chromium/patches/         ← M4: feature flag + post-decode hook + interop
├── benchmarks/               ← ms/frame tables per mode/resolution; A/B screenshots
├── install/                  ← SDK fetch scripts (NGC), driver checks, CI matrix
│   └── check_gpu.py          ← hardware gate: supported/experimental/unsupported
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
| 3 | mpv filter with keybind toggle | watch YouTube-quality stream / local file upscaled live | 1–2 wks |
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

## 10. References

- Chromium commit 46f000a (NVIDIA VP super resolution, Windows) — the GUID + hook
- docs.nvidia.com/maxine/vfx — VFX SDK guide (VSR modes, Linux driver reqs)
- catalog.ngc.nvidia.com/.../nvvfxsuperres — feature install incl. RTX 4090 example
- pypi.org/project/nvidia-vfx — Python bindings, DLPack API
- github.com/voidmainvoid95/gst-nvmaxine — GStreamer prior art (LGPL)
- github.com/chromium/chromium — `kIntelVpSuperResolution` precedent
- NVIDIA forums: "Will the Linux driver support RTX Video?" (334934) — demand signal
