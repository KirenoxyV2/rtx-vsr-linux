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
- [ ] **M2** — GStreamer element (refreshed fork of `gst-nvmaxine`) → VLC + CLI
- [ ] **M3** — mpv filter with keybind toggle (`--hwdec=nvidia`, zero-copy)
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

## Quickstart (spike)

```bash
python3 -m venv .venv && source .venv/bin/activate
pip install nvidia-vfx cupy-cuda12x[ctk] nvidia-curand-cu12
source spike/env.sh                  # points CUDA_PATH/LD_LIBRARY_PATH at pip deps
.venv/bin/python spike/bench_vsr.py            # GPU 0
.venv/bin/python spike/bench_vsr.py 1          # GPU 1
```

Requires: NVIDIA driver ≥ 570.190 (Linux), RTX GPU with Tensor Cores.
Tested target: 2× RTX 4090, driver 595.84, CUDA 13.2.

## Licensing

Our code: **MIT** (see [LICENSE](LICENSE)).
The VFX SDK and its models are NVIDIA-proprietary (NGC license) and are
**not redistributable** — this repo contains only glue code plus install
scripts that fetch the SDK. Same pattern as `gst-nvmaxine` (LGPL plugin
linking a proprietary library).

## Layout

```
docs/DESIGN.md        full design doc (architecture, risks, references)
spike/                M1 benchmark harness
gstreamer/            M2 GStreamer element
mpv/                  M3 mpv filter
chromium/             M4 compositor patch + interop prototype
install/              SDK fetch scripts, driver checks
```
