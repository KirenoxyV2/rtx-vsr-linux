# HANDOFF — rtx-vsr-linux on RTX 2060 SUPER (Turing): findings, fixes, and continuation guide

**Written:** 2026-10-10, at the end of a session on Windows 10 + WSL2 (Ubuntu 26.04).
**Purpose:** hand this repo + this document to a fresh AI session on **Zorin OS** (real Linux) to continue implementation.
**Repo:** `F:\repos\rtx-vsr-linux` on the old machine → copy the whole folder to the Zorin machine.
Branch: `turing-and-mpv` (based on `main` of github.com/akdnexpat/rtx-vsr-linux).

---

## 0. TL;DR of what's proven

1. **No reverse engineering / decompilation is needed.** NVIDIA ships the *same*
   VSR model that lives inside the Windows driver as an official user-space Linux
   SDK: the `nvidia-vfx` pip wheel (VFX SDK). The whole project is plumbing frames
   through it. The Windows driver "magic" (D3D11 GUID call) is not needed on Linux.
2. **Turing (RTX 20xx) is fully supported by the model** — contrary to the repo's
   "experimental, thermally risky" gate. The bundled model lib contains cubins for
   sm_75 (41), sm_80 (69), sm_86 (10), sm_89 (10). All AI modes (LOW/MEDIUM/HIGH/
   ULTRA), DENOISE, TrueHDR, BICUBIC load and run on an RTX 2060 SUPER.
3. **Real-time budgets on the 2060 SUPER** (measured, `spike/results_turing.txt`):
   - 720p→1440p: 245 / 231 / 112 fps (LOW/MED/HIGH)
   - 1080p→4K: 103 / 49 / 32 fps (MED/HIGH/ULTRA)
   - VRAM cost: 0.2–0.9 GB per mode. 60 fps playback is comfortable at LOW/MEDIUM.
4. **The hardware gate in `install/check_gpu.py` should be relaxed**: Turing =
   supported (not experimental) — evidence is in this repo.
5. A **custom mpv 0.41.0 with the VapourSynth bridge** was built and the
   `vapoursynth` vf appears in `mpv --vf=help`. A working VSR `.vpy` script exists
   (`mpv/vsr.vpy`, verified standalone at 62 fps 640×384→1280×768 MEDIUM).
6. An **MSE-correct segment transform** was added (`proxy/vsr_segment.py`,
   exposed as `POST /transform?mse=1`): fragmented MP4, monotonic PTS==DTS,
   no B-frames. Verified with ffprobe. This is the foundation the browser
   extension needs (the old whole-clip re-encode was NOT MSE-appendable).

---

## 1. Environment findings (WSL2-specific — mostly NOT needed on Zorin)

### 1a. The WSL2 blocker and its fix (CRITICAL if anyone retries on WSL)

On WSL2, `NvVFX_Load` failed with code -1 and the process aborted with
`free(): double free detected in tcache 2`. Root cause (found via strace):

- The NGX runtime does `dlopen("libcuda.so")` — the **unversioned** soname.
- WSL's driver userspace lives in `/usr/lib/wsl/lib/` and only provides
  `libcuda.so.1`; the ldconfig cache has no unversioned entry → dlopen fails →
  effect load fails → the C++ error path double-frees at teardown.

**Fix (one-time, root):**

```bash
ln -sf /usr/lib/wsl/lib/libcuda.so.1 /usr/lib/x86_64-linux-gnu/libcuda.so
ldconfig
```

On **Zorin (native Linux)** the NVIDIA driver package installs a proper
`libcuda.so` (usually via `libcuda1` package) so this should not occur. If
`NvVFX_Load` fails with -1 on Zorin, run `strace -f -e trace=openat <probe>`
and check what `libcuda.so` resolves to — `ldconfig -p | grep libcuda` must
show an unversioned entry.

### 1b. How to verify the model's GPU-arch support (trail marker)

The wheel bundles the model at
`<site-packages>/nvvfx/libs/libnvidia-ngx-vsr.so.1.8.3` (Linux) /
`nvngx_vsr.dll` (Windows). Both embed ~130 CUDA cubin **ELF** images. To decode
arch support, scan for `\x7fELF`, check `e_machine == 0xBE` (EM_CUDA), and read
`e_flags` at offset 48 (the value IS the sm arch, e.g. 0x75=sm_75).
`spike/probe_arch.sh` contains the (strings-based, less accurate) variant;
the ELF-header method is what produced the definitive numbers above.

### 1c. Windows-side cross-check (already done, don't repeat)

`spike/probe_win.py` ran the same wheel on the same GPU under the Windows
driver: **every mode loads and runs** (BICUBIC, AI 1–4, TrueHDR). This proved
Turing compatibility independent of the Linux failure (which turned out to be
the WSL libcuda issue).

---

## 2. Environment setup recipe (portable to Zorin)

Zorin OS 17.x is Ubuntu 22.04-based. Differences from WSL/Ubuntu 26.04:
- System Python is 3.10 — use `uv` with managed 3.12 (proven working).
- **NVIDIA driver must be ≥ 570.190** (VFX SDK requirement). On the Windows
  host it was 610.88. On Zorin install via Driver Manager / `ubuntu-drivers`,
  choose a 570+ or newer branch, and verify with `nvidia-smi`.
- apt has `mpv` (too old) and possibly distro VapourSynth (R57). **Use the pip
  VapourSynth wheel (R81)** — it bundles the core lib, headers, pkgconfig, and
  vspipe. Distro VS is untested here.

```bash
# 1. uv + python 3.12
curl -LsSf https://astral.sh/uv/install.sh | sh
uv python install 3.12
uv venv --python 3.12 ~/vsr-venv

# 2. project deps (from repo root)
uv pip install --python ~/vsr-venv -r requirements.txt   # nvidia-vfx, cupy-cuda12x, PyAV, ...

# 3. VapourSynth R81 wheel (bundles libvapoursynth.so.4, libvsscript.so, headers, vspipe)
uv pip install --python ~/vsr-venv vapoursynth

# 4. Tell the VSScript embedded-interpreter where python lives (CRITICAL —
#    without this, "Could not initialize VapourSynth scripting" /
#    "Python executable and library path couldn't be determined")
~/vsr-venv/bin/vapoursynth config
# → writes ~/.config/vapoursynth/vapoursynth.toml mapping libvsscript.so to
#   (venv python, libpython3.12.so path, mtime). Do NOT hand-set PYTHONHOME
#   for vspipe/mpv; the toml is the supported mechanism.
```

Env for running VSR python (bench/proxy/scripts):

```bash
SITE=~/vsr-venv/lib/python3.12/site-packages
export LD_LIBRARY_PATH="$SITE/vapoursynth"    # vsscript/vapoursynth libs (mpv needs this)
# CUDA_PATH etc. from spike/env.sh is for the OLD venv layout — adapt if needed;
# the nvvfx wheel loads its own bundled libs itself (nvvfx/_lib_loader.py).
```

---

## 3. mpv with the VapourSynth bridge (M3) — recipe + status

### 3a. Build recipe (proven on Ubuntu 26.04; same on 22.04 with backported deps)

Ubuntu/Zorin stock mpv is built WITHOUT the vapoursynth filter. Build 0.41.0:

```bash
# deps (names valid on Ubuntu 26.04; on 22.04 check availability — libpostproc
# didn't exist as -dev, libxscrnsaver-dev is called libxss-dev):
apt install build-essential pkg-config git python3-dev meson ninja-build \
  libavutil-dev libavcodec-dev libavformat-dev libavdevice-dev \
  libswscale-dev libswresample-dev libavfilter-dev libplacebo-dev \
  libasound2-dev libpulse-dev libjack-jackd2-dev libx11-dev libxrandr-dev \
  libxext-dev libxinerama-dev libxpresent-dev libxss-dev libwayland-dev \
  wayland-protocols libxkbcommon-dev libegl1-mesa-dev libgbm-dev libdrm-dev \
  libgl1-mesa-dev libluajit-5.1-dev libass-dev libuchardet-dev librubberband-dev \
  libzimg-dev libharfbuzz-dev libfreetype-dev libfontconfig-dev zlib1g-dev \
  libjpeg-dev libbluray-dev libdvdnav-dev libcdio-dev libcdio-paranoia-dev \
  libarchive-dev libncurses-dev liblcms2-dev

git clone --depth 1 --branch v0.41.0 https://github.com/mpv-player/mpv.git

# The pip VapourSynth wheel ships vapoursynth.pc but NOT vapoursynth-script.pc,
# and mpv's meson.build requires BOTH. Synthesize the missing one:
SP=~/vsr-venv/lib/python3.12/site-packages
cat > "$SP/vapoursynth/pkgconfig/vapoursynth-script.pc" <<EOF
prefix=\${pcfiledir}/..
includedir=\${prefix}/include

Name: vapoursynth-script
Description: VapourSynth script evaluator (VSScript API)
Version: 81
Cflags: -I\${includedir}
Libs: -L\${prefix} -lvsscript
EOF

export PKG_CONFIG_PATH="$SP/vapoursynth/pkgconfig"
meson setup builddir --prefix=/opt/mpv-vsr \
  -Dvapoursynth=enabled -Dlibmpv=true \
  -Dhtml-build=disabled -Dmanpage-build=disabled
ninja -C builddir && meson install -C builddir   # install as root
```

**Pitfalls hit (don't repeat):**
- mpv 0.41 meson options are `manpage-build` (singular) and `html-build` —
  NOT `manpages-build`/`build-docs`.
- The wheel's `libvsscript.so` dlopens libpython at runtime; it found the
  uv-managed `libpython3.12.so.1.0` automatically once `vapoursynth config`
  was run (writes `~/.config/vapoursynth/vapoursynth.toml`).
- Runtime needs `LD_LIBRARY_PATH` containing `$SP/vapoursynth` (for
  `libvsscript.so` + `libvapoursynth.so.4`). See `mpv/mpv-vsr.sh` wrapper.
- Full working build script: `spike/build_mpv.sh`; pc-file script:
  `spike/make_vs_script_pc.sh`.

### 3b. The VSR VapourSynth script — `mpv/vsr.vpy` (WORKING, standalone-verified)

Usage:
```bash
VSR_MODE=2 VSR_SCALE=2.0 VSR_MAXH=2160 VSR_GPU=0 \
  mpv --vf=vapoursynth=/path/to/vsr.vpy video.mp4
```
(On the old machine: via `mpv/mpv-vsr.sh` wrapper which sets LD_LIBRARY_PATH.)

Design facts (empirically verified in `spike/vs_*` probes — now deleted; the
knowledge is here):
- mpv injects the input clip as global `video_in` in the .vpy.
- The reliable numpy interop: read planes with `np.asarray(frame[p])`
  (memoryview buffer protocol, works on readonly frames); **write** planes on
  `frame.copy()` (writable) via `np.copyto(np.asarray(out[p]), arr)`.
- Size-changing frames: `core.std.ModifyFrame(clip=template, clips=[template, src],
  selector=sel)` where `sel(n, f)` receives `f` = **list of frames** (one per
  clip, in order) and must return a `VideoFrame` of the template's size/format.
  `template = core.std.BlankClip(clip=src, width=OUT_W, height=OUT_H, format=vs.RGBS)`.
- `FrameEval` semantics (wrong turns I took): callback gets `n` positional +
  frames; must return a *clip*, not a frame; prop_src frames are readonly.
  ModifyFrame is the right tool; don't revisit FrameEval.
- VSR input: RGB **float32 CHW [0,1]** planar == exactly RGBS planes stacked:
  `x = cp.asarray(np.stack([np.asarray(f[p]) for p in range(3)]))`.
- **DLPack use-after-free guard (CRITICAL)**: `sr.run(x)` returns a result whose
  `.image` is a view into effect-owned GPU memory. MUST deep-copy:
  `out = cp.array(cp.from_dlpack(res.image))` and `del res` before further GPU
  work. Freeing the view corrupts the CUDA context *stickily* (only process
  restart clears it). This is also documented in DESIGN §9a.
- Engine warmup once per (mode, out_w, out_h); engine instances are reusable.
- YUV input from mpv: convert first with
  `core.resize.Bicubic(clip, format=vs.RGBS, matrix_in_s="709")`.

Verified standalone (`spike/test_vpy.sh`): 640×384 → 1280×768 MEDIUM at 62 fps.

### 3c. NOT yet done (first task on Zorin)

**mpv end-to-end playback was never re-tested after the VSScript config fix.**
Sequence of events: `--vf=vapoursynth` failed with "Could not initialize
VapourSynth scripting" → root-caused via vspipe ("Python executable and
library path couldn't be determined") → fixed with `vapoursynth config` →
vspipe verified working (rendered 10 frames, "12707 fps"). mpv uses the same
VSScript API, so it should now work, but the mpv re-run did NOT happen before
the session stopped. **Do this first:**

```bash
export LD_LIBRARY_PATH="$SP/vapoursynth"
unset PYTHONHOME PYTHONPATH          # let the toml drive it
/opt/mpv-vsr/bin/mpv --vo=null --ao=null --frames=30 \
  --vf=vapoursynth=<repo>/mpv/vsr.vpy <some 480p h264 file>
```
Watch for: `[vapoursynth]` errors; then test real-time playback with
`--vo=gpu` under Wayland/X11 and measure dropped frames (`mpv --term-status-msg`).
If VSScript still fails in mpv (but not vspipe), diff the env: mpv may need
`LD_LIBRARY_PATH` to also include the uv python lib dir (libpython3.12.so.1.0
location: `~/.local/share/uv/python/cpython-3.12.15-*/lib/`).

---

## 4. Proxy / browser path (M-browser) — MSE correctness

### 4a. What was wrong with the original design

- `proxy/vsr_proxy.py` (upstream) re-encodes whole clips via imageio-ffmpeg to a
  **monolithic MP4 with B-frames** → NOT appendable to MSE SourceBuffers
  (negative first-DTS, no fragmentation). The extension would break playback.
- The extension as shipped is broken outright: `extension/manifest.json`
  references `background.js` and `options.html` which **do not exist** →
  Chrome refuses to load the unpacked extension.

### 4b. What was built (uncommitted→ now committed): `proxy/vsr_segment.py`

`transform_segment(data, mode, scale, max_h, gpu, strict)`:
- Demuxes any input segment via PyAV; audio-only or ≥ cap → passthrough (None).
- Decodes → VSR on GPU (same DLPack guards) → re-encodes to **H.264** with
  `zerolatency`, `bframes=0`, `keyint=30` → muxes to **fragmented MP4**
  (`movflags=+frag_keyframe+empty_moov+default_base_moof`).
- **Copies input PTS 1:1** (frame.pts preserved) → timestamps stay continuous
  across segments.
- Exposed via `POST /transform?mse=1` in vsr_proxy.py (alongside legacy mode).
- Per-(mode,W,H) engine cache so segments of a stream reuse the loaded model.

Verified (`spike/test_segment_mse.sh`): 150-frame 640×360 segment → 1280×720,
~2.5 s on the 2060, output = fMP4 with 5 moof boxes, PTS==DTS monotonic from 0.

### 4c. Remaining work for real-browser use (design leads)

1. **Init segment protocol**: PyAV output includes `ftyp+moov` before the first
   `moof`. For a stream of appends you must: first response → append as-is
   (init+media), subsequent responses → strip init (everything before first
   `moof`) OR cache the init and always strip. Implement `split_init(data)`
   helper (planned in vsr_segment docstring, not yet written).
2. **SourceBuffer codec matching**: output is always H.264/mp4, but YouTube's
   video SourceBuffer may be `video/webm; codecs=vp9` (or AV1). Two strategies:
   a. **Restrict interception to H.264 (avc1) streams** — simplest, MVP.
      Sniff `sb.type` (SourceBuffer mime) before transforming.
   b. Create a parallel `video/mp4; codecs="avc1.640028"` SourceBuffer and
      rewrite the player's buffer appends — needs real-world YouTube testing.
3. **Extension rewrite** (files missing): minimal viable set =
   `content.js` (exists, needs mse=1 + codec gate + init-strip) +
   `background.js` (per-site toggle via chrome.action click → storage) +
   `options.html/js` (mode/scale/site list). Manifest already declares them.
4. **Segment-level real-time**: per-segment latency on the 2060 was ~17 ms/frame
   total (decode+VSR+encode CPU x264). For 1 s @ 30 fps segments that's ~0.5 s
   per segment — fine with a 2–3 segment buffer, but consider NVENC (see 4d).
5. **Timestamp offsets**: MSE appendWindow / timestampOffset handling in the
   extension: the proxy preserves input PTS, so appended segments continue the
   source timeline naturally. Verify against `SourceBuffer.timestampOffset`
   and YouTube's own aborts/seeks (chain reset logic exists in content.js).

### 4d. NVENC lead (untested)

`vsr_segment.py` already has an `VSR_ENCODER` env branch (h264_nvenc options
preset=p1, tune=ll, bf=0). NOT verified: `spike/test_nvenc.sh` probe had a bug
(wrong attribute name: it's `av.ffmpeg_version_info`), and PyAV's pip wheel may
not include nvenc encoders. Options: system ffmpeg (apt build HAS nvenc — pipe
frames via rawvideo to an `ffmpeg -c:v h264_nvenc` subprocess), or a
custom PyAV build. NVDEC for decode side similarly (PyAV wheel lacks *_cuvid).
WSL ships libnvidia-encode.so; on Zorin the driver package provides it too.

---

## 5. Numbers to keep (RTX 2060 SUPER, driver 610.88/WSL, nvidia-vfx 0.2.0.0)

| Mode | Case | ms/f | fps | VRAM delta |
|------|------|------|-----|------------|
| LOW | 720p→1440p | 4.09 | 244.7 | 198 MB |
| MEDIUM | 720p→1440p | 4.32 | 231.4 | 222 MB |
| HIGH | 720p→1440p | 8.94 | 111.9 | 336 MB |
| MEDIUM | 1080p→4K | 9.68 | 103.3 | 434 MB |
| HIGH | 1080p→4K | 20.35 | 49.1 | 700 MB |
| ULTRA | 1080p→4K | 31.49 | 31.8 | 874 MB |

4090 reference (upstream M1) is ~8–10× faster per frame. Turing gate verdict
in `install/check_gpu.py` should become "supported" with a note to prefer
LOW/MEDIUM for ≥1080p→4K sustained loads (thermal).

---

## 6. Repo state at handoff

- Branch `turing-and-mpv` on top of upstream `main`.
- Commits: Turing findings + bench + vsr.vpy (4d717dd), cleanup (29952dc),
  plus a final commit containing: `proxy/vsr_segment.py`, the `/transform?mse=1`
  wiring in `proxy/vsr_proxy.py`, `mpv/mpv-vsr.sh` wrapper, all build/test
  scripts in `spike/` (build_mpv.sh, make_vs_script_pc.sh, test_vpy.sh,
  test_segment_mse.sh, test_mse_correctness.sh, test_mpv_raw.sh,
  test_identity_vpy.sh, test_vspipe2.sh, diag_*.sh, trace_vsscript*.sh,
  probe_win.py …), and this document.
- **The machine this ran on was cleaned**: WSL packages (mpv build deps, ffmpeg,
  meson etc.), `/opt/mpv-vsr`, `~/mpv-build`, `~/vsr-venv`, uv, VapourSynth
  config, the WSL libcuda.so symlink, and the Windows-side probe venv were all
  removed. Everything must be rebuilt on Zorin from §2 + §3 recipes.

## 7. Suggested order of work on Zorin

1. Copy repo over; `git log --oneline` to confirm branch state.
2. Install NVIDIA driver ≥ 570; `nvidia-smi` sanity check; `ldconfig -p | grep libcuda`.
3. §2 env setup; run `spike/probe_turing.py` cases 0–6 (expect all ok).
4. §3a build mpv; §3c mpv end-to-end playback test (the big unverified step).
5. Real-time playback check + keybind toggle via mpv lua (`vf toggle vapoursynth=...`).
6. §4c extension rewrite + `split_init`; test on a local MSE harness page
   (serve an fMP4 via the proxy demo page) before touching YouTube.
7. NVENC/NVDEC experiment (§4d).
8. Update README/DESIGN (Turing supported; WSL note; MSE architecture),
   then PR upstream.

## 8. Oddities learned (so you don't rediscover them)

- nvidia-vfx wheel is an **sdist** on PyPI (payload downloads at install);
  versions: 0.1.0.0 / 0.1.0.1 / 0.2.0.0. Everything here was tested on 0.2.0.0.
- The wheel's libs are self-loading (`nvvfx/_lib_loader.py` RTLD_GLOBALs
  `nvvfx/libs/*` itself); you generally do NOT need to preset
  LD_LIBRARY_PATH for plain python scripts — only for mpv (libvsscript.so).
- `VideoSuperRes` accepts `quality=` as int (1–4) or `QualityLevel` enum;
  also modes 8–11 denoise, 12–15 deblur, 16–19 high-bitrate, 21/23 streaming
  (streaming modes are Ampere+ per the feature lib's own error string).
- Output sizes must be multiples of 16 (`_align16`).
- On failure inside nvvfx, the process can abort with a C++ double-free at
  teardown — run probes one-per-process (`spike/probe_turing.py` does this)
  and use `os._exit(0)` to skip interpreter teardown when probing failures.
- Windows wheel (`nvngx_vsr.dll`) and Linux wheel model are the same lineage
  (~59 MB, 130 embedded cubins, same arch mix) — no Windows-only magic.
- This session's Windows→WSL shell quoting mangled `$VARS` and `~` — irrelevant
  on Zorin, but if a future session drives one OS from the other, write
  scripts to files instead of inline shell strings.
