#!/bin/bash
# List the real meson option names for mpv v0.41 (avoid guessing).
cd /home/kirenoxy/mpv-build/mpv 2>/dev/null && meson configure builddir 2>/dev/null | grep -E "vapoursynth|libmpv|html|manpage|docs" || \
  grep -E "vapoursynth|libmpv|html|manpage" meson_options.txt
