#!/bin/bash
# Create the missing vapoursynth-script.pc (wheel ships VSScript4.h +
# libvsscript.so but no pkgconfig entry), then build mpv with vf_vapoursynth.
set -e
SP=/home/kirenoxy/vsr-venv/lib/python3.12/site-packages
VSDIR="$SP/vapoursynth"
PCDIR="$VSDIR/pkgconfig"

cat > "$PCDIR/vapoursynth-script.pc" <<EOF
prefix=\${pcfiledir}/..
includedir=\${prefix}/include

Name: vapoursynth-script
Description: VapourSynth script evaluator (VSScript API)
Version: 81
Cflags: -I\${includedir}
Libs: -L\${prefix} -lvsscript
EOF

export PKG_CONFIG_PATH="$PCDIR"
echo "== verify pkg-config resolves both =="
pkg-config --modversion vapoursynth vapoursynth-script
pkg-config --cflags vapoursynth vapoursynth-script
