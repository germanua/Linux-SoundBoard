#!/usr/bin/env bash
# Fedora build helper — installs deps and builds AppImage or RPM.

set -e

if ! grep -q "Fedora" /etc/os-release 2>/dev/null; then
    echo "Warning: This script is designed for Fedora"
    read -p "Continue anyway? (y/n) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

echo "Installing build dependencies..."

sudo dnf install -y \
    gtk4-devel \
    libadwaita-devel \
    pulseaudio-libs-devel \
    libX11-devel \
    libXi-devel \
    pkg-config \
    ImageMagick \
    cargo \
    rust \
    git \
    pipewire \
    wireplumber \
    alsa-lib-devel \
    gcc \
    gcc-c++ \
    clang \
    make \
    cmake

echo "Done."
echo ""

echo "What would you like to build?"
echo "1) AppImage"
echo "2) RPM package"
echo "3) Both"
read -p "Enter choice (1-3): " choice

build_appimage() {
    echo "Building AppImage..."
    ./packaging/linux/package-appimage.sh
    echo "Done: ./dist/linux-soundboard-x86_64.AppImage"
}

build_rpm() {
    echo "Building RPM..."
    sudo dnf install -y rpm-build rpmdevtools
    ./packaging/rpm/package-rpm.sh
    echo "Done. Install with: sudo dnf install -y ./dist/linux-soundboard-*.rpm"
}

case $choice in
    1) build_appimage ;;
    2) build_rpm ;;
    3) build_appimage; build_rpm ;;
    *) echo "Invalid choice"; exit 1 ;;
esac
