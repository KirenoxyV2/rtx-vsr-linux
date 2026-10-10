# HANDOFF — rtx-vsr-linux on an RTX 2060 SUPER

**Updated:** 2026-10-11, end of the session that replaced the Python proxy
with the app (Pop!_OS 24.04, GNOME/Wayland, driver 595.104.02, displays
DP-1 2560×1440 + DP-2 1920×1200).
**Branch:** `turing-and-mpv` (on top of `main` of github.com/akdnexpat/rtx-vsr-linux).
**Nothing from this session is committed** — it is all in the working tree:
`app/` (new), `extension/` (changed), `proxy/` and four of its tests
(deleted), `install/install_app.sh`, `spike/test_app_proxy.py` (new),
`spike/test_extension.py`, the docs.

---

## 1. Where things stand

| Piece | State |
|-------|-------|
| VSR on Turing | All modes load and run natively (`spike/probe_turing.py` 0–6 ok). |
| mpv | **Working and installed.** `mpv/vsr.vpy` + `mpv/main.lua` in the stock Flathub mpv; `Alt+v` toggles, `Alt+q` cycles modes. Untouched this session. |
| The app (`app/`) | **Working.** One Rust binary: the browser path's proxy (NVDEC → VSR → NVENC, all on the GPU, no Python) and an iced window for it. Installed as `~/.local/bin/rtx-vsr`; not started, not connected to any browser, not in autostart — those are yours to switch on. |
| Browser extension | **Working with the app**, v0.3.0: no settings of its own any more besides the site list; draws the "RTX VSR" indicator. Tested in headless Vivaldi and at the buffering level on YouTube. |
| Python proxy | **Removed** (`proxy/`, and `spike/test_segment_mse.py`, `test_downshift.py`, `test_policy.py`, `test_proxy_youtube.py`). `spike/test_app_proxy.py` and the app's unit tests cover what they did. |
| GStreamer (M2), Chromium patch (M4) | Not started. |

Design rationale and the pitfalls behind each piece: `docs/DESIGN.md` §5C
(mpv), §9f (extension), §9g (the app).

## 2. What you still have to do by hand

1. **Stop the old proxy.** A `python proxy/vsr_proxy.py` was still listening
   on port 8730 when this session ended; the app needs that port and says so
   in its window if it cannot have it.
2. Run `rtx-vsr`. The NVIDIA runtime is found in `~/vsr-venv`, so there is
   nothing to download on this machine.
3. In the window: **Browsers → Connect Vivaldi**, then restart Vivaldi. After
   that, remove the hand-loaded "RTX VSR Local Upscaler" from
   `vivaldi://extensions` (otherwise both copies relay every segment), and
   `~/Documents/extension` can go.
4. Optional: *Start at login*, *Start minimized*, *Show RTX VSR indicator*.

## 3. This machine

- No passwordless sudo: nothing here needed root, and nothing was installed
  system-wide. `ffmpeg`/`mpv`/`meson` are **not** installed on the host.
- Rust: `rustup` stable in `~/.cargo` (add `~/.cargo/bin` to PATH); with
  `rustfmt` and `clippy`. `uv` in `~/.local/bin`.
- `~/vsr-venv` — Python 3.12: `requirements.txt` + `vapoursynth` (R81). Runs
  everything in `spike/`, and holds the copy of the NVIDIA runtime the app
  uses on this machine.
- `~/vsr-venv-py313` — Python 3.13: `nvidia-vfx`, `cupy-cuda12x[ctk]`,
  `nvidia-curand-cu12`. Used *only* as site-packages by the Flatpak mpv.
  Must not contain `vapoursynth`.
- mpv: Flatpak `io.mpv.Mpv` 0.41.0;
  `~/.var/app/io.mpv.Mpv/config/mpv/scripts/rtx-vsr` → symlink to `mpv/`.
- Browser: Flatpak `com.vivaldi.Vivaldi` (Chromium 152). Its real profile
  still has the old extension loaded unpacked from `~/Documents/extension`
  (see §2).
- The app's files: settings `~/.config/rtx-vsr/config.json`; data (its own
  runtime download, exported extension) `~/.local/share/rtx-vsr/`; launcher
  `~/.local/share/applications/rtx-vsr.desktop` (written at first start);
  login entry `~/.config/autostart/rtx-vsr.desktop` (only while *Start at
  login* is on); per-browser override
  `~/.local/share/applications/<browser>.desktop` (only while connected).

Rebuild from nothing:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
install/install_app.sh        # cargo build --release, installs ~/.local/bin/rtx-vsr
# for the tests and mpv:
curl -LsSf https://astral.sh/uv/install.sh | sh
uv venv --python 3.12 ~/vsr-venv
uv pip install --python ~/vsr-venv -r requirements.txt vapoursynth
install/install_mpv.sh        # builds ~/vsr-venv-py313, links the mpv script, smoke-tests
```

## 4. Tests

The first four pass at handoff; `install_mpv.sh` was not run again this
session (nothing it covers changed).

```bash
(cd app && cargo test && cargo clippy --all-targets)   # 17 unit tests
~/vsr-venv/bin/python spike/test_app_proxy.py          # proxy vs. MSE: timing, colour, caps
~/vsr-venv/bin/python spike/test_extension.py          # extension + app in headless Vivaldi
~/vsr-venv/bin/python -W ignore spike/test_vpy.py      # mpv filter colour math + fps
install/install_mpv.sh                                 # ends with a real mpv run
```

The two end-to-end tests start their own copy of the app on ports 8742 and
8744 with a throwaway settings directory, so they run next to yours.
`test_extension.py` also uses a page server on 8731 and DevTools on 9333,
starts the browser with the command line of the launcher override that
`rtx-vsr --connect` writes (into a throwaway home), and cleans up after
itself.

To look at the window without touching your own settings or launchers:

```bash
XDG_CONFIG_HOME=/tmp/x/config XDG_DATA_HOME=/tmp/x/data \
  app/target/release/rtx-vsr --port 8743 --screenshot /tmp/x/window.png
```

(`--screenshot` saves a picture of the whole panel and quits; it is not in
`--help`.)

## 5. Traps found so far

- **`NvVFX_SetImage` keeps the pointer** to the image descriptor. Move the
  descriptor and `NvVFX_Run` fails with "pixel format is not accommodated".
- **`VideoSuperRes` accepts only 8-bit BGRA/RGBA chunky GPU images** through
  the C API; the float planar format of the Python binding is refused.
- **NVIDIA's libraries crash on orderly unload**: the app leaves with
  `_exit`; Python probes with `os._exit`.
- **Editing files of an unpacked extension changes a running browser**
  (content scripts and the service worker are read from disk when needed),
  so `~/Documents/extension` was deliberately *not* updated while the old
  proxy was in use.
- **Flatpak `/tmp` is the `/run/user/$UID` tmpfs** (1.6 GB here). A browser
  profile put in a sandboxed app's `/tmp` fills it and breaks the session.
  Share a host directory in with `--filesystem=` instead, as
  `test_extension.py` does.
- **`flatpak run` children outlive `terminate()`**; use `--die-with-parent`,
  and Chromium's zygotes can still linger — sweep by the profile path.
  Never `pkill -f` a pattern that also appears in your own shell command line.
- **Headless Vivaldi**: a tab from plain `Target.createTarget` never becomes
  scriptable (use `newWindow: true`), attaching before the navigation commits
  hangs `Runtime.evaluate`, and `--load-extension` needs
  `--disable-features=DisableLoadExtensionCommandLineSwitch`.
- **Headless YouTube pauses itself**, with or without the extension, and
  picks 240p; sustained playback can only be judged in a real window.
- **GNOME on Wayland gives no way to inspect windows** from a script
  (`Shell.Introspect` is denied). Minimising was checked under XWayland with
  `xprop` and on Wayland by tracing the protocol (`WAYLAND_DEBUG=1`).
- Output sizes need only be even with nvidia-vfx 0.2.0.0, and fractional
  factors work; the model stops at 4×.

## 6. What to do next

1. **Use it for real**: a long YouTube session in a normal window — seeking,
   quality switches, ads, live streams, a video dragged from one display to
   the other. Watch the window's "Last upscaled" line and the indicator.
2. **A tray icon** (StatusNotifierItem) so that the app can run without a
   window where the desktop has a tray; this GNOME session has none, which is
   why "Start minimized" minimises and closing the window quits.
3. **VP9 through NVDEC** (Turing decodes it): the extension could then stop
   steering players to H.264, which caps YouTube sources at 1080p.
4. **Packaging**: a release binary on GitHub, or a Flatpak, so that building
   with cargo is not step one.
5. **mpv**: HDR sources (stay out of the way when the transfer is PQ/HLG);
   zero-copy is a native-filter job.
6. **Housekeeping before a PR upstream**: `spike/` still holds WSL-only
   debugging scripts with hard-coded `/mnt/f` paths (`diag_*.sh`,
   `trace_vsscript*.sh`, `test_vspipe*.sh`, `test_mpv_raw.sh`,
   `test_identity_vpy.sh`, `test_nvenc.sh`, `probe_arch.sh`, `build_mpv.sh`,
   `make_vs_script_pc.sh`) and `spike/env.sh`, none of which are needed any
   more.

## 7. Reference: earlier findings that still matter

- **No reverse engineering needed**: NVIDIA ships the VSR model as the
  `nvidia-vfx` wheel (0.2.0.0 tested; real wheels are on pypi.nvidia.com, the
  PyPI entry is a stub that fetches them). The model library
  `nvvfx/libs/libnvidia-ngx-vsr.so.1.8.3` embeds ~130 CUDA cubins: sm_75 (41),
  sm_80 (69), sm_86 (10), sm_89 (10).
- **WSL2 only**: `NvVFX_Load` fails with -1 and a double free because NGX
  dlopens the unversioned `libcuda.so`, which WSL does not provide. Fix:
  `ln -sf /usr/lib/wsl/lib/libcuda.so.1 /usr/lib/x86_64-linux-gnu/libcuda.so && ldconfig`.
- Quality levels: 1–4 LOW…ULTRA; 8–11 denoise, 12–15 deblur, 16–19
  high-bitrate, 21/23 streaming (Ampere+ only).
- Raw model speed on the 2060 SUPER (`spike/results_turing.txt`):

  | Mode | Case | ms/frame | VRAM |
  |------|------|----------|------|
  | LOW | 720p→1440p | 4.1 | 198 MB |
  | MEDIUM | 720p→1440p | 4.3 | 222 MB |
  | HIGH | 720p→1440p | 8.9 | 336 MB |
  | MEDIUM | 1080p→4K | 9.7 | 434 MB |
  | HIGH | 1080p→4K | 20.4 | 700 MB |
  | ULTRA | 1080p→4K | 31.5 | 874 MB |

- Whole app pipeline on the same card, output 1440p: 720p MEDIUM 7.5 ms/frame,
  HIGH 10.9, ULTRA 15.7; 1080p MEDIUM 10.9, HIGH 19.5, ULTRA 30.4.
- **Custom mpv build** (only if the Flatpak is not an option): mpv 0.41.0
  with `-Dvapoursynth=enabled`; the pip VapourSynth wheel lacks
  `vapoursynth-script.pc`, which must be synthesised
  (`spike/make_vs_script_pc.sh`); run `vapoursynth config` once so VSScript
  finds its Python; mpv then needs `LD_LIBRARY_PATH=<site-packages>/vapoursynth`.
  Full script: `spike/build_mpv.sh` (paths are WSL ones). With such a build use
  `install/install_mpv.sh --native` and `VSR_PYTHON=<its python version>`.
