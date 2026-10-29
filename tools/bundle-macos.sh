#!/bin/sh
# Builds "LscSync.app" for every Mac (Apple Silicon and Intel) in
# target/release/bundle/, and with --dmg a disk image to share it.
# usage: ./tools/bundle-macos.sh [--skip-build] [--dmg]
#   --skip-build  reuse binaries already in target/<triple>/release (CI)
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
SKIP_BUILD=false
DMG=false
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=true ;;
        --dmg) DMG=true ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

BUNDLE=target/release/bundle
APP="$BUNDLE/LscSync.app"

# Oldest supported macOS: 12.3, the first with ScreenCaptureKit (linked for
# Music mode, which itself needs 13 for system sound; 12 uses the microphone).
export MACOSX_DEPLOYMENT_TARGET=12.3
if [ "$SKIP_BUILD" = false ]; then
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release --locked -p lscsync-app --target "$target"
    done
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# One universal binary for both architectures.
lipo -create -output "$APP/Contents/MacOS/lscsync" \
    target/aarch64-apple-darwin/release/lscsync \
    target/x86_64-apple-darwin/release/lscsync
cp assets/icon/lscsync.icns "$APP/Contents/Resources/lscsync.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>LscSync</string>
    <key>CFBundleDisplayName</key><string>LscSync</string>
    <key>CFBundleIdentifier</key><string>com.github.jonag972.lscsync</string>
    <key>CFBundleExecutable</key><string>lscsync</string>
    <key>CFBundleIconFile</key><string>lscsync</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.3</string>
    <key>NSMicrophoneUsageDescription</key><string>Sur macOS 12, le mode Musique écoute la musique de la pièce avec le micro pour faire pulser tes panneaux.</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

# Ad hoc signature: macOS keeps the Screen Recording permission across
# launches (without it, neither screen capture nor Music mode work).
codesign --force --deep --sign - "$APP"

echo "bundle: $APP"

if [ "$DMG" = true ]; then
    # Disk image with a shortcut to Applications: install by drag and drop.
    # dmgbuild writes the window layout itself, without driving Finder, so it
    # also works on CI runners. Installed once in its own environment.
    DMGBUILD=target/dmgbuild/bin/dmgbuild
    if [ ! -x "$DMGBUILD" ]; then
        python3 -m venv target/dmgbuild
        target/dmgbuild/bin/pip install --quiet --disable-pip-version-check dmgbuild==1.6.7
    fi
    DMG="$BUNDLE/LscSync-$VERSION-macos.dmg"
    rm -f "$DMG"
    "$DMGBUILD" -s installer/dmg-settings.py \
        -D app="$APP" \
        -D icon=assets/icon/lscsync.icns \
        -D background=assets/installer/dmg-background.tiff \
        "LscSync" "$DMG"
    echo "disk image: $DMG"
fi
