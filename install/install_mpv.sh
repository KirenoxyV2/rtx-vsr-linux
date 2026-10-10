#!/usr/bin/env bash
# install_mpv.sh — set up RTX VSR inside mpv.
#
# Needs an mpv with the VapourSynth filter. The Flathub build has it
# (flatpak install flathub io.mpv.Mpv); distro packages usually don't.
#
# What this does:
#   1. finds the Python that mpv's VapourSynth bridge embeds,
#   2. builds a venv for exactly that Python version holding nvidia-vfx + cupy
#      (mpv/vsr.vpy finds it by itself — mpv needs no environment setup),
#   3. links mpv/ into mpv's scripts dir as `rtx-vsr` (Alt+v toggles VSR),
#   4. plays a few frames through the filter to prove it works.
#
# Usage: install/install_mpv.sh [--flatpak | --native] [--uninstall]
#   VSR_PYTHON=3.12   override the embedded Python version (native mpv only)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP=io.mpv.Mpv
PATH="$HOME/.local/bin:$PATH"

flavour="" uninstall=0
for a in "$@"; do
    case "$a" in
        --flatpak) flavour=flatpak ;;
        --native) flavour=native ;;
        --uninstall) uninstall=1 ;;
        *) echo "unknown option: $a" >&2; exit 2 ;;
    esac
done
if [ -z "$flavour" ]; then
    if flatpak info "$APP" >/dev/null 2>&1; then flavour=flatpak
    elif command -v mpv >/dev/null; then flavour=native
    else echo "No mpv found. Install one with: flatpak install flathub $APP" >&2; exit 1
    fi
fi

if [ "$flavour" = flatpak ]; then
    mpv() { flatpak run "$APP" "$@"; }
    inpy() { flatpak run --command=python3 "$APP" "$@"; }
    scripts="$HOME/.var/app/$APP/config/mpv/scripts"
else
    inpy() { python3 "$@"; }
    scripts="${XDG_CONFIG_HOME:-$HOME/.config}/mpv/scripts"
fi

if [ "$uninstall" = 1 ]; then
    rm -fv "$scripts/rtx-vsr"
    echo "Removed the mpv script. The venvs (~/vsr-venv-py*) are left in place."
    exit 0
fi

echo "==> mpv ($flavour)"
mpv --vf=help | grep -q vapoursynth || {
    echo "This mpv has no vapoursynth filter. Use the Flathub build:" >&2
    echo "  flatpak install flathub $APP" >&2
    exit 1
}

echo "==> GPU"
python3 "$REPO/install/check_gpu.py" 2>/dev/null || exit 1

pyver="${VSR_PYTHON:-$(inpy -c 'import sys, vapoursynth; print("%d.%d" % sys.version_info[:2])')}"
venv="$HOME/vsr-venv-py${pyver/./}"
echo "==> Python $pyver runtime in $venv"
command -v uv >/dev/null || {
    echo "uv is required: curl -LsSf https://astral.sh/uv/install.sh | sh" >&2
    exit 1
}
[ -d "$venv" ] || uv venv --python "$pyver" "$venv"
# No `vapoursynth` here on purpose: the script must use mpv's own module.
uv pip install --python "$venv" 'nvidia-vfx>=0.1.0' 'cupy-cuda12x[ctk]>=13' nvidia-curand-cu12

echo "==> mpv script -> $scripts/rtx-vsr"
mkdir -p "$scripts"
ln -sfn "$REPO/mpv" "$scripts/rtx-vsr"

echo "==> smoke test"
log="$(mpv --no-config --vo=null --ao=null --frames=5 --msg-level=vf=v \
    "--vf=vapoursynth=file=$REPO/mpv/vsr.vpy:user-data=\"mode=2,scale=2\"" \
    "$REPO/spike/sample_720p.mp4" 2>&1 | tr '\r' '\n')" || true
if grep -q '\[out\] 2560x1440' <<<"$log"; then
    echo "OK: 1280x720 -> 2560x1440 through RTX VSR."
    echo "In mpv: Alt+v toggles VSR, Alt+q cycles LOW/MEDIUM/HIGH/ULTRA."
else
    grep -E 'vapoursynth|Disabling|rror' <<<"$log" >&2 || true
    echo "FAILED: the filter did not produce upscaled frames (log above)." >&2
    [ "$flavour" = flatpak ] && echo "If the sandbox can't read $REPO or $venv, grant it:" \
        "flatpak override --user --filesystem=host:ro $APP" >&2
    exit 1
fi
