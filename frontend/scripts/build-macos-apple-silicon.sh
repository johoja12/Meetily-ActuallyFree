#!/bin/bash

set -euo pipefail

TARGET="aarch64-apple-darwin"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FRONTEND_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$FRONTEND_DIR/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This build must run on macOS."
    exit 1
fi

command -v cargo >/dev/null || { echo "Rust is required."; exit 1; }
command -v pnpm >/dev/null || { echo "pnpm is required."; exit 1; }

rustup target add "$TARGET"

echo "Building the Apple Silicon llama-helper sidecar..."
(
    cd "$REPO_DIR"
    cargo build --release --package llama-helper --target "$TARGET" --features metal
)

mkdir -p "$FRONTEND_DIR/src-tauri/binaries"
cp \
    "$REPO_DIR/target/$TARGET/release/llama-helper" \
    "$FRONTEND_DIR/src-tauri/binaries/llama-helper-$TARGET"
chmod +x "$FRONTEND_DIR/src-tauri/binaries/llama-helper-$TARGET"

# Release packaging needs the DMG (the default). Local installs can pass
# MEETILY_MAC_BUNDLES=app to skip the disk-image step.
BUNDLES="${MEETILY_MAC_BUNDLES:-dmg}"
echo "Building the Apple Silicon bundle ($BUNDLES)..."
(
    cd "$FRONTEND_DIR"
    pnpm exec tauri build --target "$TARGET" --bundles "$BUNDLES"
)

echo "Build complete."
if [[ "$BUNDLES" == *dmg* ]]; then
    find "$REPO_DIR/target/$TARGET/release/bundle/dmg" -maxdepth 1 -name "*.dmg" -print
else
    find "$REPO_DIR/target/$TARGET/release/bundle/macos" -maxdepth 1 -name "*.app" -print
fi
