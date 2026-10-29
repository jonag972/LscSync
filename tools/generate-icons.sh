#!/bin/sh
# Regenerates every icon variant from assets/icon/lscsync.svg.
# Run on macOS (AppKit renders the SVG, iconutil builds the .icns).
set -eu
cd "$(dirname "$0")/.."

SVG=assets/icon/lscsync.svg
OUT=assets/icon
RENDER="swift tools/render-svg.swift"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Reference PNGs (README, Dock icon in development).
$RENDER "$SVG" "$OUT/lscsync-1024.png" 1024
$RENDER "$SVG" "$OUT/lscsync-256.png" 256

# macOS: .icns (Apple grid, margin included).
ICONSET="$TMP/lscsync.iconset"
mkdir "$ICONSET"
for size in 16 32 128 256 512; do
    $RENDER "$SVG" "$ICONSET/icon_${size}x${size}.png" "$size"
    $RENDER "$SVG" "$ICONSET/icon_${size}x${size}@2x.png" $((size * 2))
done
iconutil -c icns "$ICONSET" -o "$OUT/lscsync.icns"

# Windows: multi-size .ico, cropped (Windows icons have no macOS margin).
for size in 16 24 32 48 64 128 256; do
    $RENDER "$SVG" "$TMP/win-$size.png" "$size" 7 7 86 86
done
python3 - "$TMP" "$OUT/lscsync.ico" <<'EOF'
import struct, sys
tmp, out = sys.argv[1], sys.argv[2]
sizes = [16, 24, 32, 48, 64, 128, 256]
pngs = [open(f"{tmp}/win-{s}.png", "rb").read() for s in sizes]
header = struct.pack("<HHH", 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
entries, data = b"", b""
for size, png in zip(sizes, pngs):
    entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png), offset + len(data))
    data += png
open(out, "wb").write(header + entries + data)
EOF

# GitHub social preview (Settings › Social preview): 1280×640.
$RENDER "$SVG" "$TMP/social-icon.png" 400
python3 - "$TMP/social-icon.png" "$OUT/social-preview.svg" <<'EOF'
import base64, sys
icon = base64.b64encode(open(sys.argv[1], "rb").read()).decode()
open(sys.argv[2], "w").write(f'''<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="640" viewBox="0 0 1280 640">
<rect width="1280" height="640" fill="#0B0B0D"/>
<image href="data:image/png;base64,{icon}" x="120" y="120" width="400" height="400"/>
<text x="560" y="300" fill="#F2F1EF" font-family="Helvetica Neue, Arial" font-size="84" font-weight="600">LscSync</text>
<text x="562" y="370" fill="#8E8D89" font-family="Helvetica Neue, Arial" font-size="27">Your LSC panels follow your screen and your music</text>
<rect x="562" y="410" width="72" height="8" rx="4" fill="#D9662B"/><rect x="646" y="410" width="72" height="8" rx="4" fill="#7A4FD0"/><rect x="730" y="410" width="72" height="8" rx="4" fill="#1FA38A"/>
</svg>''')
EOF
$RENDER "$OUT/social-preview.svg" "$OUT/social-preview.png" 1280x640
rm "$OUT/social-preview.svg"

echo "icons generated in $OUT"
