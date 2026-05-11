#!/usr/bin/env bash
set -euo pipefail

# Bundle FFmpeg shared libraries and CLI binary for Linux .deb packaging.
# This script copies FFmpeg files into the cargo-bundle output structure.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."
TARGET="${1:-x86_64-unknown-linux-gnu}"

DEB_DIR=$(find "$ROOT/target/$TARGET/release/bundle/deb" -name "*.deb" -exec dirname {} \; 2>/dev/null | head -1)
if [ -z "$DEB_DIR" ]; then
    echo "Error: No .deb found in target/$TARGET/release/bundle/deb/"
    echo "Run 'cargo bundle --release' first."
    exit 1
fi

DEB_FILE=$(find "$ROOT/target/$TARGET/release/bundle/deb" -name "*.deb" | head -1)
echo "Repacking .deb with bundled FFmpeg: $DEB_FILE"

WORK_DIR=$(mktemp -d)
trap 'rm -rf "$WORK_DIR"' EXIT

# Extract .deb
dpkg-deb -R "$DEB_FILE" "$WORK_DIR/pkg"

# Create lib directory for bundled FFmpeg
LIB_DIR="$WORK_DIR/pkg/usr/lib/linewise-desktop"
mkdir -p "$LIB_DIR"

# Copy ffmpeg binary
FFMPEG_BIN=$(which ffmpeg)
cp "$FFMPEG_BIN" "$LIB_DIR/ffmpeg"
chmod +x "$LIB_DIR/ffmpeg"
echo "  Copied ffmpeg binary"

# Copy required shared libraries
REQUIRED_LIBS=(
    libavcodec
    libavformat
    libavutil
    libswscale
    libswresample
    libavfilter
    libavdevice
)
# libpostproc is GPL-only and not shipped by every FFmpeg build.
OPTIONAL_LIBS=(
    libpostproc
)

copy_so () {
    local lib="$1"
    local required="$2"
    local so_file
    so_file=$(ldconfig -p | grep "$lib" | grep "x86-64" | awk '{print $NF}' | head -1)
    if [ -z "$so_file" ]; then
        so_file=$(find /usr/lib -name "${lib}.so*" | head -1)
    fi
    if [ -n "$so_file" ] && [ -f "$so_file" ]; then
        cp -L "$so_file" "$LIB_DIR/"
        echo "  Copied $(basename "$so_file")"
        return
    fi
    if [ "$required" = "1" ]; then
        echo "Error: required library $lib not found"
        exit 1
    fi
    echo "  Skipped $lib (not present on this system)"
}

for lib in "${REQUIRED_LIBS[@]}"; do copy_so "$lib" 1; done
for lib in "${OPTIONAL_LIBS[@]}"; do copy_so "$lib" 0; done

# Patch RPATH on the main binary
BIN_PATH="$WORK_DIR/pkg/usr/bin/linewise-desktop"
if command -v patchelf &>/dev/null && [ -f "$BIN_PATH" ]; then
    patchelf --set-rpath '$ORIGIN/../lib/linewise-desktop' "$BIN_PATH"
    echo "  Patched RPATH on binary"
fi

# Patch RPATH on bundled ffmpeg
if command -v patchelf &>/dev/null; then
    patchelf --set-rpath '$ORIGIN' "$LIB_DIR/ffmpeg"
    echo "  Patched RPATH on bundled ffmpeg"
fi

# Repack .deb
dpkg-deb -b "$WORK_DIR/pkg" "$DEB_FILE"
echo "Done: $DEB_FILE (with bundled FFmpeg)"
