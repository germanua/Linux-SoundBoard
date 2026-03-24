#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
DIST_ROOT="$REPO_ROOT/dist"

cd "$REPO_ROOT"

echo "Building Debian package..."

# Check for required tools
if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "Error: dpkg-deb not found. Install with: sudo apt install dpkg-dev"
    exit 1
fi

if ! command -v debhelper >/dev/null 2>&1; then
    echo "Error: debhelper not found. Install with: sudo apt install debhelper"
    exit 1
fi

# Clean previous builds
rm -rf debian/linux-soundboard
rm -f "$DIST_ROOT"/*.deb
rm -f ../*.deb ../*.buildinfo ../*.changes

# Build using debhelper
echo "Running dpkg-buildpackage..."
dpkg-buildpackage -us -uc -b

# Move .deb to dist/
mkdir -p "$DIST_ROOT"
mv ../*.deb "$DIST_ROOT/" 2>/dev/null || true

# Clean up build artifacts
rm -f ../*.buildinfo ../*.changes

echo ""
echo "✓ Debian package created successfully:"
ls -lh "$DIST_ROOT"/*.deb
