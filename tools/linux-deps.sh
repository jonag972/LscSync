#!/bin/sh
# Installs the system libraries needed to build the app on Debian / Ubuntu.
# usage: ./tools/linux-deps.sh
set -eu
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
    pkg-config clang libclang-dev \
    libfontconfig-dev libfreetype-dev \
    libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libx11-xcb-dev libxcb1-dev libxrandr-dev \
    libvulkan-dev libegl-dev libgbm-dev libdrm-dev \
    libdbus-1-dev libpipewire-0.3-dev libspa-0.2-dev \
    libpulse-dev
