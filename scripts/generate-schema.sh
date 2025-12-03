#!/bin/bash
# Generate the Barba configuration JSON schema and save it to the repository root.
# This script should be run after building the release binary.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
SCHEMA_FILE="$ROOT_DIR/barba.schema.json"

echo "Generating Barba configuration schema..."

# Check if the release binary exists
BINARY="$ROOT_DIR/src-tauri/target/release/barba"
if [ ! -f "$BINARY" ]; then
	echo "Release binary not found at $BINARY"
	echo "Building release binary..."
	cd "$ROOT_DIR"
	cargo build --manifest-path src-tauri/Cargo.toml --release
fi

# Generate the schema
"$BINARY" generate-schema >"$SCHEMA_FILE"

echo "Schema saved to $SCHEMA_FILE"

# Validate the generated JSON
if command -v jq &>/dev/null; then
	if jq empty "$SCHEMA_FILE" 2>/dev/null; then
		echo "Schema is valid JSON"
	else
		echo "Warning: Generated schema may not be valid JSON"
		exit 1
	fi
fi

echo "Done!"
