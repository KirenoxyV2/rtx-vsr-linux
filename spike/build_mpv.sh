#!/bin/bash
# Build mpv v0.41.0 with vf_vapoursynth enabled, linking the pip VapourSynth
# (headers + libvapoursynth.so.4 bundled in the wheel), installed to /opt/mpv-vsr.
#
# Stage 1 (as root):      build_mpv.sh deps
# Stage 2 (as user):      build_mpv.sh build
set -e

SP=/home/kirenoxy/vsr-venv/lib/python3.12/site-packages
SRC=/home/kirenoxy/mpv-build
VS_PC="$SP/vapoursynth/pkgconfig"

case "${1:-}" in
deps)
  echo "== installing build deps =="
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    build-essential pkg-config git python3-dev meson ninja-build \
    libavutil-dev libavcodec-dev libavformat-dev libavdevice-dev \
    libswscale-dev libswresample-dev libavfilter-dev \
    libplacebo-dev libasound2-dev libpulse-dev libjack-jackd2-dev \
    libx11-dev libxrandr-dev libxext-dev libxinerama-dev libxpresent-dev \
    libxss-dev libwayland-dev wayland-protocols libxkbcommon-dev \
    libegl1-mesa-dev libgbm-dev libdrm-dev libgl1-mesa-dev \
    libluajit-5.1-dev libass-dev libuchardet-dev librubberband-dev \
    libzimg-dev libharfbuzz-dev libfreetype-dev libfontconfig-dev \
    zlib1g-dev libjpeg-dev libbluray-dev libdvdnav-dev libcdio-dev \
    libcdio-paranoia-dev libarchive-dev libncurses-dev liblcms2-dev
  echo "deps done"
  ;;

build)
  echo "== fetching mpv source =="
  mkdir -p "$SRC"
  cd "$SRC"
  if [ ! -d mpv ]; then
    git clone --depth 1 --branch v0.41.0 https://github.com/mpv-player/mpv.git
  fi
  cd mpv

  echo "== configure (vapoursynth enabled via wheel pkgconfig) =="
  rm -rf builddir
  export PKG_CONFIG_PATH="$VS_PC"
  meson setup builddir \
    --prefix=/opt/mpv-vsr \
    -Dvapoursynth=enabled \
    -Dlibmpv=true \
    -Dhtml-build=disabled -Dmanpages-build=disabled

  echo "== compile =="
  ninja -C builddir

  echo "== install to /opt/mpv-vsr =="
  sudo -n /usr/bin/env LD_LIBRARY_PATH="$SP/vapoursynth" \
    meson install -C builddir 2>/dev/null \
    || sudo -n meson install -C builddir

  echo "== verify =="
  /opt/mpv-vsr/bin/mpv --vf=help 2>&1 | grep -i vapoursynth || echo "VAPOURSYNTH FILTER MISSING"
  ldd /opt/mpv-vsr/lib/x86_64-linux-gnu/libmpv.so 2>/dev/null | grep -i vapour \
    || ldd /opt/mpv-vsr/bin/mpv | grep -i vapour
  echo "BUILD DONE"
  ;;

*)
  echo "usage: build_mpv.sh deps|build"
  exit 2
  ;;
esac
