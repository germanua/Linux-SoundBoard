#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
DIST_ROOT="$REPO_ROOT/dist"

cd "$SCRIPT_DIR"

echo "Building Flatpak package..."

# Check for required tools
if ! command -v flatpak-builder >/dev/null 2>&1; then
    echo "Error: flatpak-builder not found."
    echo "Install with: sudo apt install flatpak-builder (Ubuntu/Debian)"
    echo "           or: sudo dnf install flatpak-builder (Fedora)"
    exit 1
fi

# Ensure Flathub repo is added
if ! flatpak remote-list | grep -q flathub; then
    echo "Adding Flathub repository..."
    flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
fi

# Install GNOME SDK 47
echo "Checking for GNOME SDK 47..."
if ! flatpak list | grep -q "org.gnome.Sdk.*47"; then
    echo "Installing GNOME SDK 47..."
    flatpak install -y flathub org.gnome.Platform//47 org.gnome.Sdk//47 org.freedesktop.Sdk.Extension.rust-stable//23.08
else
    echo "✓ GNOME SDK 47 already installed"
fi

# Generate cargo sources for offline build
echo "Generating Cargo dependency sources..."
if [ ! -f "$REPO_ROOT/src/Cargo.lock" ]; then
    echo "Error: Cargo.lock not found. Run 'cargo build' first."
    exit 1
fi

# Download flatpak-cargo-generator if not present
if [ ! -f "$SCRIPT_DIR/flatpak-cargo-generator.py" ]; then
    echo "Downloading flatpak-cargo-generator..."
    curl -o "$SCRIPT_DIR/flatpak-cargo-generator.py" \
        https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
    chmod +x "$SCRIPT_DIR/flatpak-cargo-generator.py"
fi

# Generate cargo-sources.json
if [ ! -f "$SCRIPT_DIR/cargo-sources.json" ]; then
    echo "Generating cargo-sources.json..."
    python3 "$SCRIPT_DIR/flatpak-cargo-generator.py" "$REPO_ROOT/src/Cargo.lock" -o "$SCRIPT_DIR/cargo-sources.json"
else
    echo "✓ cargo-sources.json already exists"
fi

# Build Flatpak
BUILD_DIR="$REPO_ROOT/flatpak-build"
REPO_DIR="$REPO_ROOT/flatpak-repo"
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$REPO_ROOT/src/Cargo.toml" | head -n 1)"

echo "Building Flatpak with flatpak-builder..."
rm -rf "$BUILD_DIR" "$REPO_DIR"

flatpak-builder --force-clean --repo="$REPO_DIR" "$BUILD_DIR" com.linuxsoundboard.app.yml

# Export to single-file bundle
mkdir -p "$DIST_ROOT"
BUNDLE_PATH="$DIST_ROOT/linux-soundboard-$VERSION.flatpak"

echo "Creating Flatpak bundle..."
flatpak build-bundle "$REPO_DIR" "$BUNDLE_PATH" com.linuxsoundboard.app

echo ""
echo "✓ Flatpak package created successfully:"
ls -lh "$BUNDLE_PATH"
echo ""
echo "To install locally:"
echo "  flatpak install $BUNDLE_PATH"
echo ""
echo "To test:"
echo "  flatpak run com.linuxsoundboard.app"
