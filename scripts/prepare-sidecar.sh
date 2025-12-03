#!/bin/bash
# Script to prepare media-control sidecar binary and its dependencies for Tauri bundling.
# This copies the media-control binary and its lib directory with the required target triple suffix.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARIES_DIR="$SCRIPT_DIR/../packages/desktop/tauri/binaries"
RESOURCES_DIR="$SCRIPT_DIR/../packages/desktop/tauri/resources"

# Get the current target triple
TARGET_TRIPLE=$(rustc -vV | grep host | cut -f2 -d' ')

# Check if media-control is installed
if ! command -v media-control &>/dev/null; then
	echo "Error: media-control is not installed."
	echo "Install it with: brew install media-control"
	exit 1
fi

# Get the Homebrew prefix for media-control
MEDIA_CONTROL_PREFIX=$(brew --prefix media-control 2>/dev/null)
if [ -z "$MEDIA_CONTROL_PREFIX" ]; then
	echo "Error: Could not find media-control Homebrew prefix"
	exit 1
fi

# Copy the binary with the target triple suffix
DEST_BINARY="$BINARIES_DIR/media-control-$TARGET_TRIPLE"
rm -f "$DEST_BINARY"
cp "$MEDIA_CONTROL_PREFIX/bin/media-control" "$DEST_BINARY"
chmod +x "$DEST_BINARY"
echo "✓ Copied media-control binary to $DEST_BINARY"

# Copy the lib directory (contains mediaremote-adapter.pl and framework)
LIB_SRC="$MEDIA_CONTROL_PREFIX/lib/media-control"
LIB_DEST="$RESOURCES_DIR/lib/media-control"
rm -rf "$LIB_DEST"
mkdir -p "$LIB_DEST"
cp -R "$LIB_SRC/"* "$LIB_DEST/"
# Fix permissions (Homebrew files may be read-only)
chmod -R u+rw "$LIB_DEST"
echo "✓ Copied media-control lib to $LIB_DEST"

# For development: create lib directory in target/ so the sidecar can find its resources
# The sidecar binary is at target/debug/media-control and looks for ../lib/media-control
# which resolves to target/lib/media-control (not target/debug/lib/)
TARGET_LIB="$SCRIPT_DIR/../target/lib/media-control"

rm -rf "$TARGET_LIB"
mkdir -p "$(dirname "$TARGET_LIB")"
cp -R "$LIB_SRC" "$TARGET_LIB"
# Fix permissions for development copy too
chmod -R u+rw "$TARGET_LIB"
echo "✓ Copied media-control lib to target/lib for development"

echo ""
echo "Done! The sidecar and its resources are ready for bundling."
