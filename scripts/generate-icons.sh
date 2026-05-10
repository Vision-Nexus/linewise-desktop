#!/usr/bin/env bash
set -euo pipefail

# Generate platform-specific icons from logo.svg
# Requires: rsvg-convert (librsvg), ImageMagick (convert), iconutil (macOS only)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ICONS_DIR="$SCRIPT_DIR/../assets/icons"
SVG="$ICONS_DIR/logo.svg"

if [ ! -f "$SVG" ]; then
    echo "Error: $SVG not found"
    exit 1
fi

echo "Generating icons from $SVG..."

# Generate PNG at 1024x1024 (master size)
rsvg-convert -w 1024 -h 1024 "$SVG" -o "$ICONS_DIR/icon-1024.png"

# Generate icon.png (512x512 for Linux/general use)
rsvg-convert -w 512 -h 512 "$SVG" -o "$ICONS_DIR/icon.png"

# Generate Windows .ico (multi-size)
convert "$ICONS_DIR/icon-1024.png" \
    -define icon:auto-resize=256,128,64,48,32,16 \
    "$ICONS_DIR/icon.ico"
echo "  Created icon.ico"

# Generate macOS .icns
if command -v iconutil &>/dev/null; then
    ICONSET="$ICONS_DIR/icon.iconset"
    mkdir -p "$ICONSET"
    for size in 16 32 64 128 256 512; do
        rsvg-convert -w "$size" -h "$size" "$SVG" -o "$ICONSET/icon_${size}x${size}.png"
        double=$((size * 2))
        rsvg-convert -w "$double" -h "$double" "$SVG" -o "$ICONSET/icon_${size}x${size}@2x.png"
    done
    iconutil -c icns "$ICONSET" -o "$ICONS_DIR/icon.icns"
    rm -rf "$ICONSET"
    echo "  Created icon.icns"
else
    # Fallback: use png2icns if available, otherwise skip
    if command -v png2icns &>/dev/null; then
        rsvg-convert -w 1024 -h 1024 "$SVG" -o "$ICONS_DIR/tmp_1024.png"
        png2icns "$ICONS_DIR/icon.icns" "$ICONS_DIR/tmp_1024.png"
        rm "$ICONS_DIR/tmp_1024.png"
        echo "  Created icon.icns (via png2icns)"
    else
        echo "  Skipped icon.icns (iconutil/png2icns not available — generate on macOS)"
    fi
fi

echo "Done. Icons are in $ICONS_DIR/"
