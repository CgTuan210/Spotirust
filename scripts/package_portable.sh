#!/usr/bin/env bash
set -euo pipefail

# Build a self-contained portable Spotirust release archive.
# Optional maintainer environment variables:
#   SPOTIRUST_CLIENT_ID      - Spotify OAuth PKCE public Client ID (baked at compile time)
#   SPOTIRUST_REDIRECT_PORT  - Optional fixed loopback port (0 = dynamic port on 127.0.0.1)
#   SPOTIRUST_FEATURES       - Optional extra Cargo features (e.g. "librespot-backend")

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

EXTRA_ARGS=()
if [[ -n "${SPOTIRUST_FEATURES:-}" ]]; then
    EXTRA_ARGS+=(--features "$SPOTIRUST_FEATURES")
fi

echo "==> Building Spotirust release binary..."
cargo build --release "${EXTRA_ARGS[@]}"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
STAGE_DIR="$ROOT_DIR/target/portable/spotirust"
ARCHIVE="$ROOT_DIR/target/portable/spotirust-${OS}-${ARCH}-portable.tar.gz"

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"

cp "$ROOT_DIR/target/release/spotirust" "$STAGE_DIR/"
cp "$ROOT_DIR/README.md" "$ROOT_DIR/LICENSE" "$STAGE_DIR/"
for doc in PRODUCTION_READINESS.md PERFORMANCE_REPORT.md; do
    if [[ -f "$ROOT_DIR/$doc" ]]; then
        cp "$ROOT_DIR/$doc" "$STAGE_DIR/"
    fi
done
cp -r "$ROOT_DIR/assets" "$STAGE_DIR/"
cp -r "$ROOT_DIR/docs" "$STAGE_DIR/"
touch "$STAGE_DIR/portable.flag"

tar -czf "$ARCHIVE" -C "$ROOT_DIR/target/portable" spotirust
echo "==> Portable bundle created: $ARCHIVE"
ls -lh "$ARCHIVE"
