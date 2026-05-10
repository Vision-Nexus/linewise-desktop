#!/usr/bin/env bash
set -euo pipefail

# Bundle FFmpeg dylibs and CLI binary into the macOS .app bundle.
# Usage: ./bundle-ffmpeg-macos.sh <target-triple>
# Example: ./bundle-ffmpeg-macos.sh aarch64-apple-darwin

TARGET="${1:?Usage: $0 <target-triple>}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."
APP_BUNDLE="$ROOT/target/$TARGET/release/bundle/osx/Linewise Desktop.app"

if [ ! -d "$APP_BUNDLE" ]; then
    echo "Error: App bundle not found at $APP_BUNDLE"
    exit 1
fi

FRAMEWORKS="$APP_BUNDLE/Contents/Frameworks"
RESOURCES="$APP_BUNDLE/Contents/Resources"
MACOS_BIN="$APP_BUNDLE/Contents/MacOS/linewise-desktop"
mkdir -p "$FRAMEWORKS" "$RESOURCES"

FFMPEG_PREFIX="${FFMPEG_DIR:-$(brew --prefix ffmpeg)}"
echo "Using FFmpeg from: $FFMPEG_PREFIX"

# Copy ffmpeg CLI binary
cp "$FFMPEG_PREFIX/bin/ffmpeg" "$RESOURCES/ffmpeg"
chmod +x "$RESOURCES/ffmpeg"
echo "  Copied ffmpeg binary → Resources/"

# Copy required dylibs
DYLIBS=(
    libavcodec
    libavformat
    libavutil
    libswscale
    libswresample
    libavfilter
)

for lib in "${DYLIBS[@]}"; do
    dylib=$(find "$FFMPEG_PREFIX/lib" -name "${lib}*.dylib" -not -name "*.*.*.dylib" | head -1)
    if [ -n "$dylib" ]; then
        cp "$dylib" "$FRAMEWORKS/"
        echo "  Copied $(basename "$dylib") → Frameworks/"
    fi
done

# Fix dylib references to use @rpath
for dylib in "$FRAMEWORKS"/*.dylib; do
    basename_dylib=$(basename "$dylib")
    install_name_tool -id "@rpath/$basename_dylib" "$dylib" 2>/dev/null || true

    # Fix inter-library references
    for dep in "${DYLIBS[@]}"; do
        old_path=$(otool -L "$dylib" | grep "$dep" | awk '{print $1}' | grep -v "@rpath" || true)
        if [ -n "$old_path" ]; then
            dep_basename=$(basename "$old_path")
            install_name_tool -change "$old_path" "@rpath/$dep_basename" "$dylib" 2>/dev/null || true
        fi
    done
done

# Add rpath to main binary
install_name_tool -add_rpath "@executable_path/../Frameworks" "$MACOS_BIN" 2>/dev/null || true

# Fix ffmpeg binary references too
for dep in "${DYLIBS[@]}"; do
    old_path=$(otool -L "$RESOURCES/ffmpeg" | grep "$dep" | awk '{print $1}' | grep -v "@rpath" || true)
    if [ -n "$old_path" ]; then
        dep_basename=$(basename "$old_path")
        install_name_tool -change "$old_path" "@rpath/$dep_basename" "$RESOURCES/ffmpeg" 2>/dev/null || true
    fi
done
install_name_tool -add_rpath "@executable_path/../Frameworks" "$RESOURCES/ffmpeg" 2>/dev/null || true

echo "Done bundling FFmpeg into $APP_BUNDLE"

# Optionally create DMG
if [ "${CREATE_DMG:-0}" = "1" ]; then
    DMG_PATH="$ROOT/target/linewise-desktop-macos-${TARGET}.dmg"
    hdiutil create -volname "Linewise Desktop" \
        -srcfolder "$APP_BUNDLE" \
        -ov -format UDZO \
        "$DMG_PATH"
    echo "Created DMG: $DMG_PATH"
fi
