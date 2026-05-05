#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../common.sh"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
DIST_ROOT="$REPO_ROOT/dist"

cd "$REPO_ROOT"

echo "Building RPM package..."

require_cmd rpmbuild "Install with: sudo dnf install rpm-build" || exit 1

VERSION="$(cargo_version_from_manifest "$REPO_ROOT/src/Cargo.toml")" || exit 1

# Setup RPM build directory
RPMBUILD_DIR="$HOME/rpmbuild"
mkdir -p "$RPMBUILD_DIR"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# Create source tarball
echo "Creating source tarball..."
TARBALL="$RPMBUILD_DIR/SOURCES/linux-soundboard-$VERSION.tar.gz"
git archive --format=tar.gz --prefix="linux-soundboard-$VERSION/" HEAD > "$TARBALL" 2>/dev/null || \
    tar czf "$TARBALL" --transform "s,^,linux-soundboard-$VERSION/," \
        --exclude='.git' --exclude='target' --exclude='dist' --exclude='pkg' .

# Copy spec file
cp "$SCRIPT_DIR/linux-soundboard.spec" "$RPMBUILD_DIR/SPECS/"

# Build RPM
echo "Building RPM with rpmbuild..."
rpmbuild -ba "$RPMBUILD_DIR/SPECS/linux-soundboard.spec"

# Copy to dist/
mkdir -p "$DIST_ROOT"
cp "$RPMBUILD_DIR/RPMS/x86_64/linux-soundboard-$VERSION-"*.rpm "$DIST_ROOT/" 2>/dev/null || true

echo ""
echo "✓ RPM package created successfully:"
ls -lh "$DIST_ROOT"/*.rpm 2>/dev/null || echo "No RPM files found in dist/"
