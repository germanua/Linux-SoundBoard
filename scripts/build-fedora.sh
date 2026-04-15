#!/usr/bin/env bash
# Complete Fedora build script with automatic dependency installation

set -e

echo "=== Linux Soundboard v1.1.0 - Fedora Build Script ==="
echo ""

# Check if we're on Fedora
if ! grep -q "Fedora" /etc/os-release 2>/dev/null; then
    echo "⚠️  Warning: This script is designed for Fedora"
    read -p "Continue anyway? (y/n) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

echo "📦 Installing all build dependencies..."
echo ""

# Install all required dependencies
sudo dnf install -y \
    gtk4-devel \
    libadwaita-devel \
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

echo ""
echo "✅ All dependencies installed!"
echo ""

# Ask what to build
echo "What would you like to build?"
echo "1) AppImage (portable, works on any distro)"
echo "2) RPM package (native Fedora package)"
echo "3) Both"
read -p "Enter choice (1-3): " choice

build_appimage() {
    echo ""
    echo "🔨 Building AppImage..."
    ./packaging/linux/package-appimage.sh
    
    echo ""
    echo "✅ AppImage built successfully!"
    echo "   Location: ./dist/linux-soundboard-x86_64.AppImage"
    echo ""
    echo "To run: ./dist/linux-soundboard-x86_64.AppImage"
}

build_rpm() {
    echo ""
    echo "🔨 Building RPM package..."
    
    # Install RPM build tools if not present
    sudo dnf install -y rpm-build rpmdevtools
    
    ./packaging/rpm/package-rpm.sh
    
    echo ""
    echo "✅ RPM built successfully!"
    echo "   Location: ./dist/linux-soundboard-1.1.0-1.fc*.x86_64.rpm"
    echo ""
    echo "To install: sudo dnf install -y ./dist/linux-soundboard-1.1.0-1.fc*.x86_64.rpm"
    echo "To run: linux-soundboard"
}

case $choice in
    1)
        build_appimage
        ;;
    2)
        build_rpm
        ;;
    3)
        build_appimage
        build_rpm
        ;;
    *)
        echo "Invalid choice"
        exit 1
        ;;
esac

echo ""
echo "=== Build Complete ==="
echo ""
echo "📋 Testing checklist:"
echo "  1. Launch the application"
echo "  2. Check Wayland support: echo \$WAYLAND_DISPLAY"
echo "  3. Test virtual microphone: wpctl status -n | grep Linux_Soundboard"
echo "  4. Test audio playback"
echo "  5. Test global hotkeys"
echo ""
echo "🐛 Report issues: https://github.com/germanua/Linux-SoundBoard/issues"
