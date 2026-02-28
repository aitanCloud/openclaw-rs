#!/bin/bash
# Build the frontend TypeScript into a single bundled JS file.
# Requires: npx (Node.js) with esbuild available.
#
# Usage:
#   cd crates/openclaw-ui
#   ./build.sh
#
# The output is written to src/static/app.js, which is embedded in the
# Rust binary via include_str!().

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

echo "Building frontend..."
npx esbuild src/ts/app.ts \
    --bundle \
    --outfile=src/static/app.js \
    --format=esm \
    --target=es2022 \
    --minify

echo "Done. Output: src/static/app.js"
