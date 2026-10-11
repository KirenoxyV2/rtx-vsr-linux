# HANDOFF — rtx-vsr-linux on an RTX 2060 SUPER

**Updated:** 2026-10-11, end of the session that stopped upscaled video from
freezing: fragments come back in parts, the mode follows measured speed frame
by frame, one model is loaded at a time (Pop!_OS 24.04, GNOME/Wayland, driver
595.104.02, displays DP-1 2560×1440 at 165 Hz + DP-2 1920×1200).
**Branch:** `turing-and-mpv` (on top of `main` of github.com/akdnexpat/rtx-vsr-linux).
Everything is committed; nothing is pushed.

---

## 1. Where things stand

| Piece | State |
|-------|-------|
| VSR on Turing | All modes load and run natively (`spike/probe_turing.py` 0–6 ok). |
| mpv | **Working and installed.** `mpv/vsr.vpy` + `mpv/main.lua` in the stock Flathub mpv; `Alt+v` toggles, `Alt+q` cycles modes. Untouched this session. |
| The app (`app/`) | **Working.** One Rust binary: the browser path's proxy (NVDEC → VSR → NVENC, all on the GPU, no Python), an iced window for it and a tray icon. An upscaled fragment is handed back in parts, the first after about half a second; the VSR mode is the best one that is measured to be faster than the video plays, and a player that is running dry gets LOW until it has 2 s buffered again (`docs/DESIGN.md` §9g, *Keeping up with playback*). Closing the window leaves it in the tray; *Start minimized* starts it there at login. Its switches slide and blend when they change (`app/src/toggle.rs`). Installed as `~/.local/bin/rtx-vsr`; the launcher and the login entry on this machine run `app/target/release/rtx-vsr`, the build itself. |
| Browser extension | **Working with the app**, v0.3.0: appends each part of a fragment as it arrives and tells the app where the playhead is; no settings of its own any more besides the site list; shows the "RTX VSR" indicator (`extension/icons/indicator.png`, 70 % opaque) only while upscaled frames are playing — it fades out on pause, at the end, while buffering, over parts that were not upscaled, and when the app is off or gone — and tells the app when upscaled video is playing and on which screen. Tested in headless Vivaldi and at the buffering level on YouTube. |
| Python proxy | **Removed** (`proxy/`, and `spike/test_segment_mse.py`, `test_downshift.py`, `test_policy.py`, `test_proxy_youtube.py`). `spike/test_app_proxy.py` and the app's unit tests cover what they did. |
| GStreamer (M2), Chromium patch (M4) | Not started. |

Design rationale and the pitfalls behind each piece: `docs/DESIGN.md` §5C
(mpv), §9f (extension), §9g (the app).

## 2. What you still have to do by hand

The new build is in place (`app/target/release/rtx-vsr`, which the launcher
and the login entry run, and `~/.local/bin/rtx-vsr`). Neither the app nor
Vivaldi was running when this session ended.

1. **Start the app first, then the browser.** The extension changed with the
   app (parts, the playhead's position): starting the app refreshes the
   extension files of a connected browser, and the browser loads them when
   it starts. A browser that was already running keeps the old extension,
   which still works with the new app but gets whole fragments, as before.
2. Not checked: whether the hand-loaded "RTX VSR Local Upscaler" is still in
   `vivaldi://extensions`. If it is, remove it (otherwise both copies relay
   every segment); `~/Documents/extension` can go with it.
3. **Watch a real YouTube video on ULTRA**, which is what froze before:
   start, seek around, 1080p60, double speed. The tests play a local stream
   like a player does; YouTube's own player has not been watched.

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
- The tray: GNOME has none of its own. The "AppIndicator and
  KStatusNotifierItem Support" extension (`appindicatorsupport@rgcjonas.gmail.com`,
  v66) was installed for this user from extensions.gnome.org on 2026-10-11
  and is enabled; it lives in `~/.local/share/gnome-shell/extensions/`.
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

The first four pass at handoff; `test_tray.py`, `test_vpy.py` and
`install_mpv.sh` were not run this session (nothing they cover changed).
`test_playback.py` is new: it plays 5 s segments (720p60, 1080p30, 1080p60,
1080p30 at double speed) through the extension with the app on ULTRA at
2560×1440, the page behaving like a player, and fails if the first frame
takes more than 2 s, a seek into unbuffered video more than 3.5 s, or
playback stands still for more than 0.25 s. The build before this session
fails all three. With `--window` it runs on the desktop and also counts
frames that reach the screen late; `--off` gives the browser's own figures
to compare with, `--play 60 --ahead 30` a longer session.

```bash
(cd app && cargo test && cargo clippy --all-targets)   # 22 unit tests
~/vsr-venv/bin/python spike/test_app_proxy.py          # proxy vs. MSE: timing, colour, caps, parts
~/vsr-venv/bin/python spike/test_extension.py          # extension + app in headless Vivaldi
~/vsr-venv/bin/python spike/test_playback.py           # no stalls, quick start and seek (port 8747)
~/vsr-venv/bin/python spike/test_tray.py               # tray icon, close to tray, start minimized
~/vsr-venv/bin/python -W ignore spike/test_vpy.py      # mpv filter colour math + fps
install/install_mpv.sh                                 # ends with a real mpv run
```

The end-to-end tests start their own copy of the app on ports 8742, 8744
and 8747 with a throwaway settings directory, so they run next to yours.
`test_extension.py` also uses a page server on 8731 and DevTools on 9333,
starts the browser with the command line of the launcher override that
`rtx-vsr --connect` writes (into a throwaway home), and cleans up after
itself. `test_tray.py` (port 8745) runs the app under XWayland, where a
script can see and close windows, and plays the desktop's tray over D-Bus;
the app's windows show up on screen for a moment while it runs.

To look at the window without touching your own settings, launchers or a
copy of the app that is running:

```bash
mkdir -p /tmp/x/run
WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/tmp/x/run \
XDG_CONFIG_HOME=/tmp/x/config XDG_DATA_HOME=/tmp/x/data \
  app/target/release/rtx-vsr --port 8743 --screenshot /tmp/x/window.png
```

(`--screenshot` saves a picture of the whole panel and quits; it is not in
`--help`. The runtime directory holds the socket a second launch finds the
first one through: without one of its own, this would only bring up the
running app's window. Wayland's socket is in the real one, hence XWayland.)

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
  (`Shell.Introspect` is denied). Window behaviour is checked under XWayland
  (`WAYLAND_DISPLAY=` in the app's environment) with `xwininfo`/`xprop` and a
  `WM_DELETE_WINDOW` message for the close button, and on Wayland by tracing
  the protocol (`WAYLAND_DEBUG=1`).
- **A switch's animation can be watched from a script** under XWayland:
  `XGetImage` on the app's window returns its pixels, fast enough to see the
  frames in between. The switches were checked that way (flipped through
  the tray's menu); there is no test for it in the repository.
- **Media events do not bubble**, but a capturing listener on `document`
  gets them; the indicator relies on that for `pause`, `playing`, `waiting`.
- **ksni does not call `watcher_online` for a tray that is already there** at
  start, only for one that appears later; the app assumes a tray until
  `watcher_offline` says otherwise.
- **A GNOME extension installed from a zip is not seen until the next login**
  on Wayland. `org.gnome.Shell.Extensions.InstallRemoteExtension` over D-Bus
  makes the running shell fetch and load it (it asks on screen first).
- Output sizes need only be even with nvidia-vfx 0.2.0.0, and fractional
  factors work; the model stops at 4×.
- **VSR models that are loaded together keep their GPU memory** after they
  are dropped; one at a time they give it all back. The app holds one.
- **A headless browser shows no frames**: stalls, starts and seeks can be
  measured there, frames that reach the screen late only with `--window`.
  On the 165 Hz display 2–4 % of frames are late with upscaling off, too.

## 6. What to do next

1. **Use it for real**: a long YouTube session in a normal window — seeking,
   quality switches, ads, live streams, a video dragged from one display to
   the other. Watch the window's "Last upscaled" line and the indicator.
2. **Look at the tray icon, the window and the indicator's fade with your
   own eyes**: the tests prove the behaviour, not the looks. The icon is
   the green app mark at 64 px, scaled by the panel; the indicator fades
   over 250 ms and the switches travel in 180 ms (`FADE_MS` in
   `extension/inject.js`, `TRAVEL` in `app/src/toggle.rs`).
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
  HIGH 10.9, ULTRA 15.7; 1080p MEDIUM 10.9, HIGH 19.5, ULTRA 30.4. (Measured
  before the encoder ran alongside the model; not measured again since.)
- **Custom mpv build** (only if the Flatpak is not an option): mpv 0.41.0
  with `-Dvapoursynth=enabled`; the pip VapourSynth wheel lacks
  `vapoursynth-script.pc`, which must be synthesised
  (`spike/make_vs_script_pc.sh`); run `vapoursynth config` once so VSScript
  finds its Python; mpv then needs `LD_LIBRARY_PATH=<site-packages>/vapoursynth`.
  Full script: `spike/build_mpv.sh` (paths are WSL ones). With such a build use
  `install/install_mpv.sh --native` and `VSR_PYTHON=<its python version>`.
