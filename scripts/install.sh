#!/usr/bin/env bash
# Universal installer for Linux Soundboard
# Auto-detects distribution and installs appropriate package

set -e

VERSION="1.1.0"
REPO="germanua/Linux-SoundBoard"
GITHUB_URL="https://github.com/$REPO"

echo "=== Linux Soundboard Installer ==="
echo ""

# Detect distribution
if [ -f /etc/os-release ]; then
    . /etc/os-release
    DISTRO=$ID
    DISTRO_VERSION=$VERSION_ID
else
    echo "❌ Cannot detect distribution"
    exit 1
fi

echo "Detected: $PRETTY_NAME"
echo ""

# Function to download file
download_file() {
    local url=$1
    local output=$2
    
    if command -v wget >/dev/null 2>&1; then
        wget -q --show-progress "$url" -O "$output"
    elif command -v curl >/dev/null 2>&1; then
        curl -L --progress-bar "$url" -o "$output"
    else
        echo "❌ Neither wget nor curl found. Please install one of them."
        exit 1
    fi
}

# Function to install DEB package
install_deb() {
    echo "📦 Installing DEB package..."
    
    local deb_file="linux-soundboard_${VERSION}-1_amd64.deb"
    local download_url="$GITHUB_URL/releases/latest/download/$deb_file"
    
    echo "Downloading $deb_file..."
    download_file "$download_url" "/tmp/$deb_file"
    
    echo "Installing package..."
    sudo apt install -y "/tmp/$deb_file"
    
    rm "/tmp/$deb_file"
    
    echo "✅ Installation complete!"
    echo "Run: linux-soundboard"
}

# Function to install RPM package
install_rpm() {
    echo "📦 Installing RPM package..."
    
    local rpm_file="linux-soundboard-${VERSION}-1.fc40.x86_64.rpm"
    local download_url="$GITHUB_URL/releases/latest/download/$rpm_file"
    
    echo "Downloading $rpm_file..."
    download_file "$download_url" "/tmp/$rpm_file"
    
    echo "Installing package..."
    sudo dnf install -y "/tmp/$rpm_file"
    
    rm "/tmp/$rpm_file"
    
    echo "✅ Installation complete!"
    echo "Run: linux-soundboard"
}

# Function to install AppImage
install_appimage() {
    echo "📦 Installing AppImage..."
    
    local appimage_file="linux-soundboard-x86_64.AppImage"
    local download_url="$GITHUB_URL/releases/latest/download/$appimage_file"
    local install_dir="$HOME/.local/bin"
    
    mkdir -p "$install_dir"
    
    echo "Downloading $appimage_file..."
    download_file "$download_url" "$install_dir/$appimage_file"
    
    chmod +x "$install_dir/$appimage_file"
    
    # Create symlink
    ln -sf "$install_dir/$appimage_file" "$install_dir/linux-soundboard"
    
    # Check if ~/.local/bin is in PATH
    if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
        echo ""
        echo "⚠️  Add ~/.local/bin to your PATH:"
        echo "   echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc"
        echo "   source ~/.bashrc"
    fi
    
    echo "✅ Installation complete!"
    echo "Run: linux-soundboard"
}

# Function to install via AUR
install_aur() {
    echo "📦 Installing from AUR..."
    
    if command -v yay >/dev/null 2>&1; then
        yay -S linux-soundboard-git
    elif command -v paru >/dev/null 2>&1; then
        paru -S linux-soundboard-git
    else
        echo "❌ Neither yay nor paru found."
        echo ""
        echo "Please install an AUR helper first:"
        echo "  https://wiki.archlinux.org/title/AUR_helpers"
        exit 1
    fi
    
    echo "✅ Installation complete!"
    echo "Run: linux-soundboard"
}

# Main installation logic
case "$DISTRO" in
    ubuntu|debian|linuxmint|pop)
        install_deb
        ;;
    
    fedora|rhel|centos|rocky|almalinux)
        install_rpm
        ;;
    
    arch|manjaro|endeavouros)
        install_aur
        ;;
    
    opensuse*|sles)
        echo "📦 openSUSE detected"
        echo ""
        echo "Choose installation method:"
        echo "1) AppImage (recommended)"
        echo "2) Build from source"
        read -p "Enter choice (1-2): " choice
        
        case $choice in
            1) install_appimage ;;
            2)
                echo "Building from source..."
                echo "See: $GITHUB_URL#build-from-source"
                ;;
            *) echo "Invalid choice"; exit 1 ;;
        esac
        ;;
    
    *)
        echo "⚠️  Distribution '$DISTRO' not directly supported"
        echo ""
        echo "Available options:"
        echo "1) AppImage (universal, recommended)"
        echo "2) Flatpak (universal)"
        echo "3) Build from source"
        read -p "Enter choice (1-3): " choice
        
        case $choice in
            1) install_appimage ;;
            2)
                echo "Flatpak installation:"
                echo "  flatpak install $GITHUB_URL/releases/latest/download/linux-soundboard-${VERSION}.flatpak"
                ;;
            3)
                echo "Build from source:"
                echo "  $GITHUB_URL#build-from-source"
                ;;
            *) echo "Invalid choice"; exit 1 ;;
        esac
        ;;
esac

echo ""
echo "📚 Documentation:"
echo "  Troubleshooting: $GITHUB_URL/blob/main/docs/TROUBLESHOOTING.md"
echo "  Contributing: $GITHUB_URL/blob/main/docs/CONTRIBUTING.md"
echo ""
echo "🐛 Report issues: $GITHUB_URL/issues"
echo ""
echo "Thank you for using Linux Soundboard! 🎵"
