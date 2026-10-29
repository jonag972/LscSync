#!/bin/sh
# Builds LscSync-<version>-x86_64.AppImage in target/release/bundle/
# (Linux): a single file that runs without installation.
# usage: ./tools/appimage.sh [--skip-build]
#   --skip-build  reuse target/release/lscsync (CI)
#
# Like most AppImages of GPU apps, it relies on the system's graphics and
# audio libraries (Vulkan, Wayland or X11, PulseAudio or PipeWire). Built on
# Ubuntu 24.04, it needs a distribution of that age or newer.
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
BUNDLE=target/release/bundle
APPDIR="$BUNDLE/LscSync.AppDir"
OUTPUT="$BUNDLE/LscSync-$VERSION-x86_64.AppImage"
# Official AppImage packaging tool.
TOOL_URL=https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
TOOL=target/appimagetool-x86_64.AppImage

if [ "${1:-}" != "--skip-build" ]; then
    cargo build --release --locked -p lscsync-app
fi

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
cp target/release/lscsync "$APPDIR/usr/bin/lscsync"
cp assets/icon/lscsync-256.png "$APPDIR/lscsync.png"
ln -s usr/bin/lscsync "$APPDIR/AppRun"
cat > "$APPDIR/lscsync.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=LscSync
Comment=LSC Smart Connect panels that follow your screen and your music
Exec=lscsync
Icon=lscsync
Categories=Utility;
EOF

if [ ! -x "$TOOL" ]; then
    curl -fsSL -o "$TOOL" "$TOOL_URL"
    chmod +x "$TOOL"
fi
rm -f "$OUTPUT"
# Extract-and-run: works without FUSE (CI runners, containers).
ARCH=x86_64 "$TOOL" --appimage-extract-and-run --no-appstream "$APPDIR" "$OUTPUT"
echo "AppImage: $OUTPUT"
