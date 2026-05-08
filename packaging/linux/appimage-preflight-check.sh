#!/usr/bin/env bash
# AppImage preflight dependency checker for Linux Soundboard

set -e

MISSING_DEPS=()
WARNINGS=()
PIPEWIRE_RUNNING=0

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
        return 1
    fi
    PIPEWIRE_RUNNING=1
    return 0
}

# Check for PulseAudio daemon or socket
check_pulseaudio() {
    local runtime_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    if [ -S "$runtime_dir/pulse/native" ] || pgrep -x pulseaudio >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

# Check for a supported audio server
check_audio_server() {
    if check_pipewire || check_pulseaudio; then
        return 0
    fi
    WARNINGS+=("No PipeWire or PulseAudio daemon detected")
    return 1
}

# Check for pactl, needed for live PulseAudio virtual mic setup
check_pactl() {
    if ! command -v pactl >/dev/null 2>&1; then
        WARNINGS+=("pactl not found; pure PulseAudio setup may require restarting the audio session after first launch")
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
check_audio_server
check_pactl
if [ "$PIPEWIRE_RUNNING" -eq 1 ]; then
    check_wireplumber
fi

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
    echo "  sudo apt install pipewire wireplumber pulseaudio-utils"
    echo "  systemctl --user enable --now pipewire wireplumber"
    echo ""
    echo "Fedora:"
    echo "  sudo dnf install pipewire wireplumber pulseaudio-utils"
    echo "  systemctl --user enable --now pipewire wireplumber"
    echo ""
    echo "Arch:"
    echo "  sudo pacman -S pipewire wireplumber libpulse"
    echo "  systemctl --user enable --now pipewire wireplumber"
    echo ""
fi

exit 0
