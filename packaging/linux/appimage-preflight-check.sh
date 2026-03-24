#!/usr/bin/env bash
# AppImage preflight dependency checker for Linux Soundboard

set -e

MISSING_DEPS=()
WARNINGS=()

# Check for FUSE (Type 2 AppImage requirement)
check_fuse() {
    if ! command -v fusermount >/dev/null 2>&1 && ! command -v fusermount3 >/dev/null 2>&1; then
        MISSING_DEPS+=("FUSE")
        return 1
    fi
    return 0
}

# Check for PipeWire daemon
check_pipewire() {
    if ! pgrep -x pipewire >/dev/null 2>&1; then
        WARNINGS+=("PipeWire daemon not running")
        return 1
    fi
    return 0
}

# Check for WirePlumber
check_wireplumber() {
    if ! pgrep -x wireplumber >/dev/null 2>&1; then
        WARNINGS+=("WirePlumber not running (recommended for PipeWire)")
        return 1
    fi
    return 0
}

# Run checks
check_fuse
check_pipewire
check_wireplumber

# Display results
if [ ${#MISSING_DEPS[@]} -gt 0 ]; then
    echo "❌ Missing required dependencies:"
    for dep in "${MISSING_DEPS[@]}"; do
        echo "  - $dep"
    done
    echo ""
    echo "Installation instructions:"
    
    if [[ " ${MISSING_DEPS[@]} " =~ " FUSE " ]]; then
        echo ""
        echo "Ubuntu/Debian:"
        echo "  sudo apt install libfuse2"
        echo ""
        echo "Fedora:"
        echo "  sudo dnf install fuse-libs"
        echo ""
        echo "Arch:"
        echo "  sudo pacman -S fuse2"
    fi
    
    exit 1
fi

if [ ${#WARNINGS[@]} -gt 0 ]; then
    echo "⚠️  Warnings:"
    for warn in "${WARNINGS[@]}"; do
        echo "  - $warn"
    done
    echo ""
    echo "The application may have limited functionality."
    echo "To enable virtual microphone:"
    echo ""
    echo "Ubuntu/Debian:"
    echo "  sudo apt install pipewire pipewire-pulse wireplumber"
    echo "  systemctl --user enable --now pipewire pipewire-pulse wireplumber"
    echo ""
    echo "Fedora:"
    echo "  sudo dnf install pipewire pipewire-pulseaudio wireplumber"
    echo "  systemctl --user enable --now pipewire pipewire-pulse wireplumber"
    echo ""
fi

exit 0
