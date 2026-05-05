#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../common.sh"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
DIST_ROOT="$REPO_ROOT/dist"
DEBIAN_DIR="$REPO_ROOT/debian"

cd "$REPO_ROOT"

echo "Building Debian package..."

require_cmd dpkg-buildpackage "Install with: sudo apt install dpkg-dev" || exit 1
require_cmd dh "Install with: sudo apt install debhelper" || exit 1

cleanup() {
    rm -rf "$DEBIAN_DIR"
    rm -f ../*.buildinfo ../*.changes
}

trap cleanup EXIT

# Stage Debian metadata where dpkg-buildpackage expects it
rm -rf "$DEBIAN_DIR"
mkdir -p "$DEBIAN_DIR"
cp -a "$SCRIPT_DIR"/. "$DEBIAN_DIR"/

# Clean previous builds
rm -rf "$DEBIAN_DIR/linux-soundboard"
rm -f "$DIST_ROOT"/*.deb
rm -f ../*.deb ../*.buildinfo ../*.changes

# Build using debhelper
echo "Running dpkg-buildpackage..."
dpkg-buildpackage -us -uc -b

# Move .deb to dist/
mkdir -p "$DIST_ROOT"
mv ../*.deb "$DIST_ROOT/" 2>/dev/null || true

echo ""
echo "✓ Debian package created successfully:"
ls -lh "$DIST_ROOT"/*.deb
