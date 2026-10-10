#!/bin/bash
# Prep for building mpv with vf_vapoursynth against the pip VapourSynth.
set -e
SP=/home/kirenoxy/vsr-venv/lib/python3.12/site-packages
echo "== vapoursynth.pc =="; cat "$SP/vapoursynth/pkgconfig/vapoursynth.pc"
echo "== headers in wheel? =="; find "$SP/vapoursynth" -name "*.h" | head
echo "== wheel RECORD hint =="; grep -oE '"[^"]*\.h"' "$SP"/vapoursynth-81.dist-info/RECORD 2>/dev/null | head
