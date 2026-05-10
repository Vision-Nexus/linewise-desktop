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

# Copy required dylibs. libavdevice is pulled in by ffmpeg-sys-next 8.1+
# even when we don't use it directly.
DYLIBS=(
    libavcodec
    libavformat
    libavutil
    libswscale
    libswresample
    libavfilter
    libavdevice
)

for lib in "${DYLIBS[@]}"; do
    # Pick the major-versioned file (e.g. libavdevice.62.dylib), not the
    # bare symlink (libavdevice.dylib) or the fully-versioned file
    # (libavdevice.62.0.100.dylib). The linker stamps the major-versioned
    # SONAME into LC_LOAD_DYLIB, so that's the exact filename we must
    # produce in Contents/Frameworks.
    dylib=$(find "$FFMPEG_PREFIX/lib" -name "${lib}.*.dylib" -not -name "*.*.*.dylib" | head -1)
    if [ -z "$dylib" ]; then
        echo "Error: could not find major-versioned ${lib}.*.dylib under $FFMPEG_PREFIX/lib"
        ls "$FFMPEG_PREFIX/lib" | grep "^${lib}" || true
        exit 1
    fi
    cp "$dylib" "$FRAMEWORKS/"
    echo "  Copied $(basename "$dylib") → Frameworks/"
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

# Rewrite the main binary's load commands from absolute Homebrew paths
# (/opt/homebrew/opt/ffmpeg/lib/...) to @rpath/..., then add the rpath
# pointing at Contents/Frameworks/. Without this, dyld tries to load
# dylibs from /opt/homebrew on the end user's machine and aborts.
for dep in "${DYLIBS[@]}"; do
    old_path=$(otool -L "$MACOS_BIN" | grep "$dep" | awk '{print $1}' | grep -v "@rpath" || true)
    if [ -n "$old_path" ]; then
        dep_basename=$(basename "$old_path")
        install_name_tool -change "$old_path" "@rpath/$dep_basename" "$MACOS_BIN"
    fi
done
install_name_tool -add_rpath "@executable_path/../Frameworks" "$MACOS_BIN" 2>/dev/null || true

# Fix ffmpeg binary references too
for dep in "${DYLIBS[@]}"; do
    old_path=$(otool -L "$RESOURCES/ffmpeg" | grep "$dep" | awk '{print $1}' | grep -v "@rpath" || true)
    if [ -n "$old_path" ]; then
        dep_basename=$(basename "$old_path")
        install_name_tool -change "$old_path" "@rpath/$dep_basename" "$RESOURCES/ffmpeg"
    fi
done
install_name_tool -add_rpath "@executable_path/../Frameworks" "$RESOURCES/ffmpeg" 2>/dev/null || true

echo "Done bundling FFmpeg into $APP_BUNDLE"

# Codesign the whole bundle. Must happen AFTER install_name_tool rewrites —
# any change to a Mach-O invalidates its signature.
# In CI the workflow imports a self-signed identity and exports
# MACOS_SIGNING_IDENTITY; locally we fall back to ad-hoc ("-").
SIGN_IDENTITY="${MACOS_SIGNING_IDENTITY:--}"
echo "Codesigning $APP_BUNDLE as: $SIGN_IDENTITY"
# Do NOT pass --options runtime. The hardened runtime enforces library
# validation — every loaded dylib must share the main binary's Team ID
# (or be Apple-signed). Self-signed certs have no Team ID, so the
# bundled FFmpeg dylibs in Contents/Frameworks/ are rejected and the
# app aborts on launch. Hardened runtime is only required for Apple
# notarization, which we're not doing.
codesign --force --deep --timestamp=none \
    --sign "$SIGN_IDENTITY" "$APP_BUNDLE"
codesign --verify --deep --strict "$APP_BUNDLE"

# Optionally create DMG
if [ "${CREATE_DMG:-0}" = "1" ]; then
    DMG_PATH="$ROOT/target/linewise-desktop-macos-${TARGET}.dmg"
    hdiutil create -volname "Linewise Desktop" \
        -srcfolder "$APP_BUNDLE" \
        -ov -format UDZO \
        "$DMG_PATH"
    echo "Created DMG: $DMG_PATH"
fi
