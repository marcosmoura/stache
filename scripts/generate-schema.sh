#!/bin/bash
# Generate the Barba configuration JSON schema and save it to the repository root.
# This script requires the Barba desktop app to be running.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
SCHEMA_FILE="$ROOT_DIR/barba.schema.json"

echo "Generating Barba configuration schema..."

# Check if the CLI binary exists (workspace builds go to root target/)
CLI_BINARY="$ROOT_DIR/target/release/barba"
if [ ! -f "$CLI_BINARY" ]; then
	echo "CLI binary not found at $CLI_BINARY"
	echo "Building CLI binary..."
	cd "$ROOT_DIR"
	cargo build --package barba-cli --release
fi

# Check if the desktop app is running by looking for the socket
SOCKET_PATH="${XDG_RUNTIME_DIR:-$HOME/.local/run}/barba.sock"
if [ ! -S "$SOCKET_PATH" ]; then
	echo "Error: Barba desktop app is not running."
	echo "Please start the desktop app first, then run this script."
	echo "(Looking for socket at: $SOCKET_PATH)"
	exit 1
fi

# Generate the schema by sending command to running app
# The schema is printed to stdout by the desktop app
"$CLI_BINARY" generate-schema >"$SCHEMA_FILE"

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
