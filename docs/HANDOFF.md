# HANDOFF — rtx-vsr-linux on an RTX 2060 SUPER

**Updated:** 2026-10-10, end of the first session on native Linux
(Pop!_OS 24.04, GNOME/Wayland, driver 595.104.02, displays 2560×1440 + 1920×1200).
The earlier Windows/WSL2 session's findings are folded in below.
**Branch:** `turing-and-mpv` (on top of `main` of github.com/akdnexpat/rtx-vsr-linux).
**Nothing from this session is committed** — it is all in the working tree.

---

## 1. Where things stand

| Piece | State |
|-------|-------|
| VSR on Turing | All modes load and run natively (`spike/probe_turing.py` 0–6 ok). `install/check_gpu.py` now says `supported`. |
| mpv | **Working and installed.** `mpv/vsr.vpy` + `mpv/main.lua` in the stock Flathub mpv; `Alt+v` toggles, `Alt+q` cycles modes. Local files and YouTube (yt-dlp) both verified. |
| Proxy, MSE path | **Working.** `POST /transform?mse=1` returns fragments that land on the original timeline (`spike/test_segment_mse.py`). |
| Browser extension | **Working on a local player; partly verified on YouTube.** Rewritten (`extension/`), tested in headless Vivaldi (`spike/test_extension.py`). |
| NVENC/NVDEC | Not started. Neither PyAV's wheel nor imageio-ffmpeg's binary has nvenc. |
| GStreamer (M2), Chromium patch (M4) | Not started. |

Design rationale and the pitfalls behind each piece: `docs/DESIGN.md` §5C (mpv)
and §9f (browser).

## 2. This machine

- No passwordless sudo: nothing here needed root, and nothing was installed
  system-wide. `ffmpeg`/`mpv`/`meson` are **not** installed on the host.
- `uv` in `~/.local/bin`. Two venvs:
  - `~/vsr-venv` — Python 3.12: `requirements.txt` + `vapoursynth` (R81) +
    `websockets`. Runs the proxy and everything in `spike/`.
  - `~/vsr-venv-py313` — Python 3.13: `nvidia-vfx`, `cupy-cuda12x[ctk]`,
    `nvidia-curand-cu12`. Used *only* as site-packages by the Flatpak mpv,
    whose embedded Python is 3.13. Must not contain `vapoursynth`.
- mpv: Flatpak `io.mpv.Mpv` 0.41.0 (VapourSynth R73 built in, bundles yt-dlp).
  `~/.var/app/io.mpv.Mpv/config/mpv/scripts/rtx-vsr` → symlink to `mpv/`.
- Browser: Flatpak `com.vivaldi.Vivaldi` (Chromium 152). The user's real
  profile loads the extension unpacked from `~/Documents/extension` — a
  **copy** of `extension/` (the Flatpak cannot read the repo). After changing
  the extension: `cp extension/* ~/Documents/extension/`, then press reload on
  its card in `vivaldi://extensions`.
- An ffmpeg binary for making test clips:
  `~/vsr-venv/bin/python -c "import imageio_ffmpeg; print(imageio_ffmpeg.get_ffmpeg_exe())"`.

Rebuild from nothing:

```bash
curl -LsSf https://astral.sh/uv/install.sh | sh
uv venv --python 3.12 ~/vsr-venv
uv pip install --python ~/vsr-venv -r requirements.txt vapoursynth websockets
install/install_mpv.sh        # builds ~/vsr-venv-py313, links the mpv script, smoke-tests
```

## 3. Tests (all pass at handoff)

```bash
for i in 0 1 2 3 4 5 6; do ~/vsr-venv/bin/python spike/probe_turing.py $i; done   # modes load
~/vsr-venv/bin/python -W ignore spike/test_vpy.py     # vsr.vpy colour math vs zimg + fps
~/vsr-venv/bin/python spike/test_segment_mse.py       # fragment timestamps / structure
~/vsr-venv/bin/python spike/test_extension.py         # extension + proxy in headless Vivaldi
install/install_mpv.sh                                # ends with a real mpv run
```

`test_extension.py` starts its own proxy on 8730, a page server on 8731 and a
browser with DevTools on 9333; it cleans all three up.

## 4. Traps found this session

- **Flatpak `/tmp` is the `/run/user/$UID` tmpfs** (1.6 GB here). A browser
  profile put in a sandboxed app's `/tmp` fills it and breaks the session
  ("bwrap: … No space left on device"). Share a host directory in with
  `--filesystem=` instead, as `test_extension.py` does.
- **`flatpak run` children outlive `terminate()`**; use `--die-with-parent`,
  and Chromium's zygotes can still linger — sweep by the profile path.
  Never `pkill -f` a pattern that also appears in your own shell command line.
- **Headless Vivaldi**: a tab from plain `Target.createTarget` never becomes
  scriptable (use `newWindow: true`), attaching before the navigation commits
  hangs `Runtime.evaluate`, and `--load-extension` needs
  `--disable-features=DisableLoadExtensionCommandLineSwitch`.
- **Line endings**: the repo arrived from Windows with CRLF in the working
  tree; fixed, and `.gitattributes` now forces LF. Git's index kept stale
  sizes afterwards (`git add -u` refreshed it; nothing was staged).
- Output sizes need only be even with nvidia-vfx 0.2.0.0 (the old "multiple
  of 16" rule is gone), and fractional factors work.
- `VideoSuperRes.run()` does not modify its input tensor; its output is a
  view into effect-owned memory — keep the result object alive until every
  read has finished (DESIGN §9a).

## 5. What to do next

1. **Try the extension in the real browser.** It is loaded (see §2); run
   `~/vsr-venv/bin/python proxy/vsr_proxy.py`, play a 720p YouTube video and
   check "Stats for nerds" (codec `avc1…`, a decoded resolution above the
   selected one) and the toolbar popup, which shows the proxy state and the
   tab's `window.__vsr.status()` counters. Watch for: stalls after seeking,
   quality switches, ads, live.
   Headless YouTube pauses itself regardless of the extension, so sustained
   playback has only been proven on the local test page.
2. **NVENC** for the proxy (DESIGN §9f, "State"): the CPU encode is now the
   bottleneck — 720p60 → 1080p is 1.4× real time. Options: system ffmpeg
   (needs `sudo apt install ffmpeg`), piping raw frames to
   `ffmpeg -c:v h264_nvenc`, or a PyAV build with nvenc.
3. **Proxy colour path**: `vsr_segment.py` still converts through swscale
   RGB24 on the CPU (BT.601 both ways, so the model sees slightly wrong hues
   although the round trip cancels). Reusing the CUDA kernels from
   `mpv/vsr.vpy` would fix that and save two conversions per frame.
4. **mpv**: HDR sources (script should stay out of the way when the transfer
   is PQ/HLG — mpv does not pass the transfer to VapourSynth, so decide in
   `main.lua` from `video-params/gamma`); zero-copy is a native-filter job.
5. **Housekeeping before a PR upstream**: `spike/` still holds WSL-only
   debugging scripts with hard-coded `/mnt/f` paths (`diag_*.sh`,
   `trace_vsscript*.sh`, `test_vspipe*.sh`, `test_mpv_raw.sh`,
   `test_identity_vpy.sh`, `test_nvenc.sh`, `probe_arch.sh`, `build_mpv.sh`,
   `make_vs_script_pc.sh`). The Flatpak route made the custom mpv build
   unnecessary; decide whether to keep the build recipe (below) or drop them.
   `spike/env.sh` points at a Python 3.11 `.venv` and is no longer needed —
   cuda-pathfinder finds the pip-shipped CUDA libraries by itself.

## 6. Reference: findings from the WSL2 session that still matter

- **No reverse engineering needed**: NVIDIA ships the VSR model as the
  `nvidia-vfx` wheel (an sdist that downloads its payload at install;
  0.2.0.0 tested). The model library `nvvfx/libs/libnvidia-ngx-vsr.so.1.8.3`
  embeds ~130 CUDA cubins: sm_75 (41), sm_80 (69), sm_86 (10), sm_89 (10).
  To re-check: scan for `\x7fELF`, `e_machine == 0xBE`, read `e_flags` at
  offset 48.
- **WSL2 only**: `NvVFX_Load` fails with -1 and a double free because NGX
  dlopens the unversioned `libcuda.so`, which WSL does not provide. Fix:
  `ln -sf /usr/lib/wsl/lib/libcuda.so.1 /usr/lib/x86_64-linux-gnu/libcuda.so && ldconfig`.
  Native drivers install `libcuda.so` themselves (confirmed here).
- `VideoSuperRes(quality=…)` takes 1–4 or a `QualityLevel`; 8–11 denoise,
  12–15 deblur, 16–19 high-bitrate, 21/23 streaming (Ampere+ only).
- A failed load can abort the process at teardown: probe one mode per
  process and leave with `os._exit(0)`.
- Raw model speed on the 2060 SUPER (`spike/results_turing.txt`):

  | Mode | Case | ms/frame | VRAM |
  |------|------|----------|------|
  | LOW | 720p→1440p | 4.1 | 198 MB |
  | MEDIUM | 720p→1440p | 4.3 | 222 MB |
  | HIGH | 720p→1440p | 8.9 | 336 MB |
  | MEDIUM | 1080p→4K | 9.7 | 434 MB |
  | HIGH | 1080p→4K | 20.4 | 700 MB |
  | ULTRA | 1080p→4K | 31.5 | 874 MB |

- **Custom mpv build** (only if the Flatpak is not an option): mpv 0.41.0
  with `-Dvapoursynth=enabled`; the pip VapourSynth wheel lacks
  `vapoursynth-script.pc`, which must be synthesised
  (`spike/make_vs_script_pc.sh`); run `vapoursynth config` once so VSScript
  finds its Python; mpv then needs `LD_LIBRARY_PATH=<site-packages>/vapoursynth`.
  Meson options are `manpage-build` / `html-build`. Full script:
  `spike/build_mpv.sh` (paths are WSL ones). With such a build use
  `install/install_mpv.sh --native` and `VSR_PYTHON=<its python version>`.
