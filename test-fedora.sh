#!/usr/bin/env bash
# Quick test script for Fedora VM

set -e

echo "=== Linux Soundboard v1.1.0 Testing Script for Fedora ==="
echo ""

# Check if we're on Fedora
if ! grep -q "Fedora" /etc/os-release; then
    echo "⚠️  Warning: This script is designed for Fedora"
    read -p "Continue anyway? (y/n) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# Clone repository if not already cloned
if [ ! -d "Linux-SoundBoard" ]; then
    echo "📥 Cloning repository..."
    git clone https://github.com/germanua/Linux-SoundBoard.git
fi

cd Linux-SoundBoard
git checkout testing
git pull origin testing

echo ""
echo "Choose what to test:"
echo "1) Build and test RPM package (recommended for Fedora)"
echo "2) Build and test AppImage"
echo "3) Both"
read -p "Enter choice (1-3): " choice

install_build_deps() {
    echo ""
    echo "📦 Installing build dependencies..."
    sudo dnf install -y rpm-build cargo rust gtk4-devel libadwaita-devel \
        pulseaudio-libs-devel libX11-devel libXi-devel pkgconfig ImageMagick git \
        pulseaudio-utils pipewire pipewire-pulseaudio wireplumber
}

test_rpm() {
    echo ""
    echo "🔨 Building RPM package..."
    ./packaging/rpm/package-rpm.sh
    
    echo ""
    echo "📦 Installing RPM..."
    sudo dnf install -y ./dist/linux-soundboard-1.1.0-1.fc*.x86_64.rpm
    
    echo ""
    echo "✅ RPM installed successfully!"
    echo ""
    echo "To run: linux-soundboard"
    echo "To uninstall: sudo dnf remove linux-soundboard"
}

test_appimage() {
    echo ""
    echo "🔨 Building AppImage..."
    ./packaging/linux/package-appimage.sh
    
    echo ""
    echo "✅ AppImage built successfully!"
    echo ""
    echo "To run: ./dist/linux-soundboard-x86_64.AppImage"
}

install_build_deps

case $choice in
    1)
        test_rpm
        ;;
    2)
        test_appimage
        ;;
    3)
        test_rpm
        test_appimage
        ;;
    *)
        echo "Invalid choice"
        exit 1
        ;;
esac

echo ""
echo "=== Testing Complete ==="
echo ""
echo "📋 What to test:"
echo "  1. Launch the application"
echo "  2. Check if it runs on Wayland (echo \$WAYLAND_DISPLAY)"
echo "  3. Test virtual microphone creation"
echo "  4. Test audio playback"
echo "  5. Test global hotkeys"
echo ""
echo "🐛 Report issues at: https://github.com/germanua/Linux-SoundBoard/issues"
