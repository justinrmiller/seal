#!/usr/bin/env bash
set -euo pipefail

# Bundle libsodium-wrappers-sumo into a single browser-ready JS file.
# Requires: node, npm

SODIUM_VERSION="0.7.15"
OUTFILE="$(cd "$(dirname "$0")/.." && pwd)/app/static/vendor/sodium.js"
TMPDIR=$(mktemp -d)

cleanup() { rm -rf "$TMPDIR"; }
trap cleanup EXIT

echo "Bundling libsodium-wrappers-sumo@${SODIUM_VERSION}..."

cd "$TMPDIR"
npm init -y --silent > /dev/null 2>&1
npm install --silent "libsodium-wrappers-sumo@${SODIUM_VERSION}" esbuild > /dev/null 2>&1

cat > entry.js << 'EOF'
const sodium = require('libsodium-wrappers-sumo');
window.sodium = sodium;
EOF

./node_modules/.bin/esbuild entry.js \
    --bundle \
    --format=iife \
    --platform=browser \
    --minify \
    --outfile="$OUTFILE"

echo "Written to $OUTFILE ($(du -h "$OUTFILE" | cut -f1 | xargs))"
