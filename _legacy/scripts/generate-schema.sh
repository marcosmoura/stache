#!/bin/bash
# Generate the Stache configuration JSON schema and save it to the repository root.
# This script uses the CLI binary which can generate the schema without the desktop app running.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
SCHEMA_FILE="$ROOT_DIR/stache.schema.json"

echo "Generating Stache configuration schema..."

# Check if the binary exists (workspace builds go to root target/)
STACHE_BINARY="$ROOT_DIR/target/release/stache"
if [ ! -f "$STACHE_BINARY" ]; then
  echo "Stache binary not found at $STACHE_BINARY"
  echo "Building binary..."
  cd "$ROOT_DIR"
  cargo build --package stache --release
fi

# Generate the schema using the CLI
# The stache binary can generate the schema without the desktop app running
"$STACHE_BINARY" schema >"$SCHEMA_FILE"

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
