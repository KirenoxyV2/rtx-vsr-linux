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
- [x] **The app** — [`app/`](app/): one Rust program that is both the local
      proxy for browser video and the window that controls it. Decode, upscale
      and encode all happen on the GPU (NVDEC → VSR → NVENC) with no Python
      anywhere: 720p → 1440p at 7.5 ms/frame on an RTX 2060 SUPER. It takes
      the target resolution from your displays, steps the quality down when
      GPU memory or speed runs short, can start at login, makes Chromium
      browsers load the extension by themselves, and can mark upscaled video
      with an "RTX VSR" indicator. See
      [Use it in the browser](#use-it-in-the-browser).
- [x] **Browser extension** — [`extension/`](extension/) hooks MSE
      `SourceBuffer.appendBuffer` on enabled sites, sends each H.264 fragment to
      the app and appends the upscaled fragment in its place (per-site
      switch). Verified in headless Chromium on a local DASH player
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

For Chromium-based browsers (Chrome, Chromium, Vivaldi, Brave, Edge…). One
program does it: the app is the local proxy that upscales the video and the
window that controls it.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust, to build the app
install/install_app.sh                                           # -> ~/.local/bin/rtx-vsr
rtx-vsr
```

Then, in the window:

1. **Status → NVIDIA runtime.** If it says "Not installed", press *Download*.
   The app fetches NVIDIA's VFX SDK from NVIDIA (pypi.nvidia.com, 422 MB,
   checksum verified) and keeps the 175 MB of libraries VSR needs. A copy that
   pip put in `~/vsr-venv` is used as it is. No Python is involved either way.
2. **Browsers → Connect ‹your browser›.** From its next start the browser loads
   the extension by itself — no developer mode, no "Load unpacked". Restart the
   browser once.
3. Play something. YouTube is enabled from the start; the extension's toolbar
   button switches other sites on and off.

| Setting | What it does |
|---------|--------------|
| Upscale video | The master switch. Off: video plays untouched. |
| Upscale to | Resolution to upscale to. The list ends at your largest display's resolution. With several displays, video playing on a smaller one is upscaled only to *that* display's resolution. |
| Quality | Low … Ultra (the model's four modes). Stepped down by itself when GPU memory is short or the card cannot keep up with playback. |
| Show RTX VSR indicator | Draws an "RTX VSR" mark in the top right corner of a video while the frames on screen are upscaled ones. |
| Start at login | Starts the app with your session. *Start minimized* (shown once this is on) then starts it in the tray without opening the window; on a desktop without a tray the window is opened minimized instead. |

The app upscales for as long as it runs. Closing the window leaves it running
in the tray: a click on the icon (or starting the app a second time) brings
the window back, and the icon's menu switches upscaling on and off and has
*Quit*. On a desktop without a tray, closing the window quits the app, and
video then simply plays as usual. GNOME gets a tray from the "AppIndicator and
KStatusNotifierItem Support" extension.

**What "Connect" does.** A running browser cannot be told to load anything,
so the app writes a launcher override: a copy of the browser's own `.desktop`
file in `~/.local/share/applications` with `--load-extension=…` added, and the
extension itself where that browser can read it (for a Flatpak, inside
`~/.var/app/<id>/`). Switching Connect off deletes both. If the browser is not
listed, or you would rather do it by hand, *Extension folder* opens a copy to
load with `chrome://extensions` → Developer mode → *Load unpacked*.

**What happens on an enabled site.** VP9/AV1 are reported as unsupported so the
player picks H.264; every `moof+mdat` fragment appended to a video SourceBuffer
goes to the app, which decodes it (NVDEC), upscales it (VSR) and encodes it
(NVENC) without the frames leaving the GPU, and comes back with the original
timestamps. Audio is untouched, and if the app is not running or fails, the
original bytes are appended instead.

Measured on an RTX 2060 SUPER, whole pipeline, output 2560×1440:

| Source | Medium | High | Ultra |
|--------|--------|------|-------|
| 720p | 7.5 ms/frame (134 fps) | 10.9 ms (92 fps) | 15.7 ms (64 fps) |
| 1080p | 10.9 ms/frame (91 fps) | 19.5 ms (51 fps) | 30.4 ms (33 fps) |

(Low is within a millisecond of Medium at these sizes.)

Limits to know about:

- H.264 only, so on YouTube the source is at most 1080p (which is the range
  worth upscaling anyway). DRM video is never touched. The model goes up to
  4×, so 240p becomes 960p, not more.
- Connect relies on Chromium's `--load-extension` switch, which takes effect
  when the browser starts and which Google has been phasing out of branded
  Chrome builds. If a browser stops honouring it, load the extension by hand.
- Verified: the tests below, and YouTube in a headless browser (240p → 960p,
  20 s buffered, indicator in place, no errors). Not yet verified: long
  YouTube sessions in a normal window — quality switches, ads, live streams.

For scripts and machines without a display there are command-line forms of
the same things: `rtx-vsr --headless`, `--install-runtime`, `--browsers`,
`--connect <browser>` (see `rtx-vsr --help`).

```bash
(cd app && cargo test)                                  # MP4, sizing, launcher patching, …
~/vsr-venv/bin/python spike/test_app_proxy.py           # the app's proxy against what MSE needs
~/vsr-venv/bin/python spike/test_extension.py           # extension + app in a headless browser
```

mpv gives the better picture path (no re-encode); the app is for staying in
the browser.

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
pip install -r requirements.txt      # nvidia-vfx, cupy, PyAV, etc.
source spike/env.sh                  # points CUDA_PATH/LD_LIBRARY_PATH at pip deps
.venv/bin/python spike/bench_vsr.py            # GPU 0
.venv/bin/python spike/bench_vsr.py 1          # GPU 1
```

Requires: NVIDIA driver ≥ 570.190 (Linux), RTX GPU with Tensor Cores.
Tested target: 2× RTX 4090, driver 595.84, CUDA 13.2.

## Licensing

Our code: **MIT** (see [LICENSE](LICENSE)).
The VFX SDK and its models are NVIDIA-proprietary (NGC license) and are
**not redistributable** — this repo contains only glue code; the app and the
install scripts fetch the SDK from NVIDIA. Same pattern as `gst-nvmaxine` (LGPL plugin
linking a proprietary library).

## Layout

```
docs/DESIGN.md        full design doc (architecture, risks, references)
app/                  the app: GPU proxy + control window (Rust, iced)
extension/            browser extension (MV3): MSE hook -> the app
spike/                M1/M1.5 benchmark harness, probes, end-to-end tests
gstreamer/            M2 GStreamer element
mpv/                  M3 mpv filter: vsr.vpy (VapourSynth) + main.lua (toggle)
chromium/             M4 compositor patch + interop prototype
install/              check_gpu.py (hardware gate), install_app.sh, install_mpv.sh
```
