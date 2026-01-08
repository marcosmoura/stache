#!/bin/bash
# Generate the Barba configuration JSON schema and save it to the repository root.
# This script uses the CLI binary which can generate the schema without the desktop app running.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
SCHEMA_FILE="$ROOT_DIR/barba.schema.json"

echo "Generating Barba configuration schema..."

# Check if the binary exists (workspace builds go to root target/)
BARBA_BINARY="$ROOT_DIR/target/release/barba"
if [ ! -f "$BARBA_BINARY" ]; then
	echo "Barba binary not found at $BARBA_BINARY"
	echo "Building binary..."
	cd "$ROOT_DIR"
	cargo build --package barba --release
fi

# Generate the schema using the CLI
# The barba binary can generate the schema without the desktop app running
"$BARBA_BINARY" schema >"$SCHEMA_FILE"

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
