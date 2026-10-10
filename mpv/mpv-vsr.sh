#!/bin/bash
# mpv-vsr — wrapper that runs the custom mpv (vapoursynth-enabled) with the
# pip VapourSynth's bundled libs AND the standalone python runtime visible:
# libvsscript.so dlopen()s libpython3.12.so.1 and the embedded interpreter
# must find the venv's site-packages for cupy/nvvfx imports in .vpy scripts.
UVPY=/home/kirenoxy/.local/share/uv/python/cpython-3.12.15-linux-x86_64-gnu
VENV=/home/kirenoxy/vsr-venv
SP="$VENV/lib/python3.12/site-packages"

export LD_LIBRARY_PATH="$SP/vapoursynth:$UVPY/lib:${LD_LIBRARY_PATH:-}"

# VSScript embedded python: point at the venv so site-packages resolve.
# Py_SetPythonHome semantics: PYTHONHOME=prefix tells it where lib/python3.x is.
export PYTHONHOME="$VENV"
export PYTHONPATH="$SP"

exec /opt/mpv-vsr/bin/mpv "$@"
