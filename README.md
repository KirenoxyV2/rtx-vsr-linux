# rtx-vsr-linux

Real-time NVIDIA RTX Video Super Resolution (VSR) on Linux — the missing piece
between Windows Chrome's "it just works" AI video upscaling and our desktop.

Windows ships the VSR model *inside the driver* and Chromium toggles it with a
single vendor GUID call. On Linux neither exists, so this project does in user
space what the Windows driver does internally: take decoded frames (already on
the GPU), run NVIDIA's official VSR model via the **VFX SDK** (`nvvfx`), and
hand the upscaled frame back to the player/compositor.

## Status

- [x] Design doc — [`docs/DESIGN.md`](docs/DESIGN.md) (how Windows works, why
  Linux lacks it, architecture options A–D, milestones)
- [x] **M1** — Spike: `nvidia-vfx` runs on consumer RTX 4090. Results in
      [`spike/results_m1.txt`](spike/results_m1.txt): LOW 720p→1440p **0.5 ms/f**,
      ULTRA 1080p→4K **3.1 ms/f** (~330 fps), ~0.2–0.6 GB VRAM, correctness
      verified (per-mode outputs differ). No NGC key needed; deps are pip-only.
- [x] **M1.5** — End-to-end pipeline probe: real decode→VSR→encode runs
      faster than real-time. GPU-resident AI path is **0.63 ms/frame**; full
      NVDEC+AI+NVENC production estimate **6.6–11.6 ms/frame = 2.5–5× headroom**
      at 30 fps. Latency not perceivable. A/B visual confirms genuine upscaling.
      Results in [`spike/results_m1_5.txt`](spike/results_m1_5.txt).
- [x] **Hardware gate** — [`install/check_gpu.py`](install/check_gpu.py): any
      RTX card, Turing (20xx) and newer; pre-Turing refused. Turing verified on
      an RTX 2060 SUPER ([`spike/results_turing.txt`](spike/results_turing.txt)).
      See [Hardware](#hardware).
- [x] **Proxy core** — [`proxy/vsr_proxy.py`](proxy/vsr_proxy.py): local HTTP
      transform service. `GET /upscale` (file source), `POST /transform` (raw
      segment bytes — the browser-as-fetcher path, see DESIGN §9d), `/health`,
      demo page. decode → VSR → H.264, GPU-serialized, DLPack-safe.
- [x] **VRAM auto-downshift** — proxy picks the highest quality mode that fits
      *live* free VRAM (ULTRA→HIGH→MEDIUM→LOW) so it coexists with a local LLM
      instead of failing; `?strict=1` for exact-mode-or-503. DESIGN §9b.
- [x] **Resolution policy** — output capped at 1440p (`--max-out`); sources
      already ≥ cap pass through with zero GPU work/VRAM. DESIGN §9c.
- [x] **Browser extension** — [`extension/`](extension/) hooks MSE
      `SourceBuffer.appendBuffer` on enabled sites, sends each H.264 fragment to
      `POST /transform?mse=1` and appends the upscaled fragment in its place
      (per-site toggle). Verified in headless Chromium on a local DASH player
      ([`spike/test_extension.py`](spike/test_extension.py)) and against
      YouTube's player at the buffering level; see
      [Use it in the browser](#use-it-in-the-browser) for what is still rough.
- [ ] **M2** — GStreamer element (refreshed fork of `gst-nvmaxine`) → VLC + CLI
- [x] **M3** — mpv filter with keybind toggle: [`mpv/`](mpv/) runs VSR inside
      the stock Flathub mpv (no custom build), `Alt+v` to toggle, auto-downshift
      when the GPU can't hold the frame rate. See [Use it in mpv](#use-it-in-mpv).
      Frames take one round trip through system memory; zero-copy is still open.
- [ ] **M4** — Chromium compositor patch: post-decode hook, DMA-BUF↔CUDA
      interop, feature flag (the real "Chrome on Linux" goal)
- [ ] **M5** — Upstream PR / distro fork patches + CI

## Why this is feasible (TL;DR)

1. Chromium's entire Windows integration is ~40 lines calling a private
   `ID3D11VideoContext::VideoProcessorSetStreamExtension` GUID — the model runs
   in the driver, frames never leave the GPU.
2. That D3D11 video path doesn't exist on Linux (DXVK's is rudimentary and has
   nowhere to forward the GUID).
3. But NVIDIA ships the same VSR model as a user-space library for Linux:
   the **VFX SDK** (`nvvfx`), with Python bindings on PyPI
   (`pip install nvidia-vfx`). Driver requirement 570.190+/580.82+/590.44+.
4. So we write the plumbing (frame import → model → frame export) that the
   Windows driver did for free. Every primitive has in-tree precedent in
   Chromium; no reverse engineering needed on the happy path.

## Use it in mpv

Works today, including YouTube (mpv plays it through yt-dlp):

```bash
flatpak install flathub io.mpv.Mpv                 # this build has the VapourSynth filter
curl -LsSf https://astral.sh/uv/install.sh | sh    # uv, for the Python runtime
install/install_mpv.sh
```

Then in mpv: **`Alt+v`** toggles VSR, **`Alt+q`** cycles LOW → MEDIUM → HIGH →
ULTRA. The video is upscaled to fit the display it is shown on; a source that
already fills the display is left alone (no GPU work). If a mode is too heavy
for the frame rate, the script steps down by itself.

```bash
flatpak run io.mpv.Mpv 'https://www.youtube.com/watch?v=...'          # then Alt+v
flatpak run io.mpv.Mpv --script-opts=vsr-auto=yes,vsr-mode=3 film.mkv  # on from the start
```

Options go in `script-opts/vsr.conf` (`auto`, `mode`, `scale`, `fmt`,
`downshift`, `gpu` — see [`mpv/main.lua`](mpv/main.lua)). The filter also works
without the script: `--vf=vapoursynth=file=mpv/vsr.vpy:user-data="mode=2"`.

How it fits together: the installer builds a venv for the Python version mpv
embeds, holding `nvidia-vfx` and `cupy`; [`mpv/vsr.vpy`](mpv/vsr.vpy) finds
that venv by itself, so mpv needs no environment variables or wrapper. Colour
conversion runs on the GPU on both sides of the model; mpv receives ordinary
10-bit YUV. Any other mpv built with `vapoursynth` works the same way
(`install/install_mpv.sh --native`).

Measured on an RTX 2060 SUPER, decode included, output 2560×1440:

| Source | LOW/MEDIUM | HIGH | ULTRA |
|--------|-----------|------|-------|
| 480p | 111 fps | — | 80 fps |
| 720p (10-bit HEVC) | 104 fps | 82 fps | 60 fps |
| 1080p | 86 fps | 49 fps | 32 fps |

HDR sources are not handled (the model is SDR); leave VSR off for those.

## Use it in the browser

For Chromium-based browsers (Chrome, Vivaldi, Brave…). Start the proxy, then
load the extension unpacked:

```bash
uv venv --python 3.12 ~/vsr-venv && uv pip install --python ~/vsr-venv -r requirements.txt
~/vsr-venv/bin/python proxy/vsr_proxy.py           # http://127.0.0.1:8730, add --max-out 2160 for 4K
```

`chrome://extensions` → Developer mode → *Load unpacked* → pick `extension/`.
YouTube is enabled by default; the toolbar button toggles the current site
(the tab reloads), and the options page sets quality, factor and site list.
A Flatpak browser cannot read the repo directory — copy `extension/` somewhere
it can, e.g. `~/Downloads`.

What happens on an enabled site: VP9/AV1 are reported as unsupported so the
player picks H.264; every `moof+mdat` fragment appended to a video
SourceBuffer is upscaled by the proxy (output capped at the screen height) and
appended with its original timestamps. Audio is untouched, and if the proxy is
down or fails, the original bytes are appended instead.

Limits to know about:

- H.264 only, so on YouTube the source is at most 1080p (which is the range
  worth upscaling anyway). DRM video is never touched.
- Decode and encode run on the CPU (x264). On a 2060 SUPER + 20-thread CPU
  720p60 → 1080p runs at ~87 fps, about 1.4× real time; segments arrive a few
  seconds later than usual, so seeking feels slower. NVENC would help.
- Verified: the local player test, and YouTube buffering 40 s of 720p60 as
  1080p with no errors. Not yet verified: long YouTube sessions in a normal
  (non-headless) window — quality switches, ads, live streams.

mpv is the more polished route today; the extension is for staying in the
browser.

## Hardware

Any RTX card works: the model library bundled with `nvidia-vfx` carries
kernels for Turing (sm_75), Ampere (sm_80/86) and Ada (sm_89). The check in
`install/check_gpu.py` (exit code + JSON verdict) gates on that:

| Tier | Cards | Verdict | Notes |
|------|-------|---------|-------|
| **Supported** | RTX 20xx (Turing, sm_75) and newer | `supported` (exit 0) | 4090 is the fast reference; a 2060 SUPER runs every mode at roughly a tenth of its speed — plenty for LOW/MEDIUM at 60 fps. |
| **Unsupported** | Pre-Turing, no NVIDIA GPU, or driver below minimum | `unsupported` (exit 20) | Refused with a clear message. |

Driver requirement: **≥ 570.190 / 580.82 / 590.44** (tested on 595.84 and
595.104). The check reads this from `nvidia-smi` and warns if the driver is
too old.

## Quickstart (spike)

```bash
python3 -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt      # nvidia-vfx, cupy, PyAV (all YT codecs), etc.
source spike/env.sh                  # points CUDA_PATH/LD_LIBRARY_PATH at pip deps
.venv/bin/python spike/bench_vsr.py            # GPU 0
.venv/bin/python spike/bench_vsr.py 1          # GPU 1
```

Requires: NVIDIA driver ≥ 570.190 (Linux), RTX GPU with Tensor Cores.
Tested target: 2× RTX 4090, driver 595.84, CUDA 13.2.

**Every YouTube stream type is supported** — the proxy decodes via PyAV
(FFmpeg), not just MP4: H.264/MP4, VP9/WebM, and AV1 all upscale (verified
through the live `/transform` endpoint). Output is always H.264 (what browsers
play back best); sources already ≥ 1440p pass through untouched.

## Licensing

Our code: **MIT** (see [LICENSE](LICENSE)).
The VFX SDK and its models are NVIDIA-proprietary (NGC license) and are
**not redistributable** — this repo contains only glue code plus install
scripts that fetch the SDK. Same pattern as `gst-nvmaxine` (LGPL plugin
linking a proprietary library).

## Layout

```
docs/DESIGN.md        full design doc (architecture, risks, references)
proxy/vsr_proxy.py    local HTTP transform service (route B core)
proxy/vsr_segment.py  MSE-correct fragment transform behind /transform?mse=1
extension/            browser extension (MV3): MSE hook -> proxy
spike/                M1/M1.5 benchmark harness + probes
gstreamer/            M2 GStreamer element
mpv/                  M3 mpv filter: vsr.vpy (VapourSynth) + main.lua (toggle)
chromium/             M4 compositor patch + interop prototype
install/              check_gpu.py (hardware gate), install_mpv.sh
```
