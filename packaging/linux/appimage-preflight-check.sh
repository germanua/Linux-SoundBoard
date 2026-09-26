#!/usr/bin/env bash


set -e

MISSING_DEPS=()
WARNINGS=()
PIPEWIRE_RUNNING=0
DISTRO_ID=""
DISTRO_LIKE=""


os_release_value() {
    local key=$1 value
    value="$(sed -n "s/^${key}=//p" /etc/os-release | head -n 1)"
    value="${value#\"}"
    value="${value%\"}"
    printf '%s\n' "$value"
}

detect_distro() {
    if [ -r /etc/os-release ]; then
        DISTRO_ID="$(os_release_value ID)"
        DISTRO_LIKE="$(os_release_value ID_LIKE)"
    fi
}

glibc_version() {
    local value
    value="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
    value="${value#glibc }"
    [[ "$value" =~ ^([0-9]+)\.([0-9]+) ]] || return 1
    printf '%s.%s
' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
}

check_platform() {
    local arch version major minor
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64) ;;
        *) MISSING_DEPS+=("x86_64 CPU/OS"); return 1 ;;
    esac
    version="$(glibc_version || true)"
    if [ -z "$version" ]; then
        MISSING_DEPS+=("glibc 2.39+")
        return 1
    fi
    major="${version%%.*}"
    minor="${version#*.}"
    if (( major < 2 || (major == 2 && minor < 39) )); then
        MISSING_DEPS+=("glibc 2.39+ (found $version)")
        return 1
    fi
    return 0
}

host_library_available() {
    local library=$1 cache=""
    if command -v ldconfig >/dev/null 2>&1; then
        cache="$(ldconfig -p 2>/dev/null || true)"
    elif [ -x /sbin/ldconfig ]; then
        cache="$(/sbin/ldconfig -p 2>/dev/null || true)"
    fi
    [ -n "$cache" ] && grep -qF "$library" <<<"$cache"
}

check_host_libraries() {
    local library
    for library in libfribidi.so.0 libharfbuzz.so.0 libfontconfig.so.1 libwayland-client.so.0 libfreetype.so.6 libX11-xcb.so.1 libX11.so.6 libpipewire-0.3.so.0 libcom_err.so.2 libgpg-error.so.0; do
        host_library_available "$library" || MISSING_DEPS+=("$library")
    done
}

check_fuse() {
    if ! command -v fusermount >/dev/null 2>&1 && ! command -v fusermount3 >/dev/null 2>&1; then
        MISSING_DEPS+=("FUSE")
        return 1
    fi
    return 0
}


check_pipewire() {
    if ! pgrep -x pipewire >/dev/null 2>&1; then
        return 1
    fi
    PIPEWIRE_RUNNING=1
    return 0
}


check_pulseaudio() {
    local runtime_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    if [ -S "$runtime_dir/pulse/native" ] || pgrep -x pulseaudio >/dev/null 2>&1; then
        return 0
    fi
    return 1
}


check_audio_server() {
    if check_pipewire || check_pulseaudio; then
        return 0
    fi
    WARNINGS+=("No PipeWire or PulseAudio daemon detected")
    return 1
}


check_pactl() {
    if ! command -v pactl >/dev/null 2>&1; then
        WARNINGS+=("pactl not found; pure PulseAudio setup may require restarting the audio session after first launch")
        return 1
    fi
    return 0
}


check_wireplumber() {
    if ! pgrep -x wireplumber >/dev/null 2>&1; then
        WARNINGS+=("WirePlumber not running (recommended for PipeWire)")
        return 1
    fi
    return 0
}


detect_distro
check_platform || true
check_host_libraries
check_fuse || true
check_audio_server || true
check_pactl || true
if [ "$PIPEWIRE_RUNNING" -eq 1 ]; then
    check_wireplumber || true
fi


if [ ${#MISSING_DEPS[@]} -gt 0 ]; then
    echo "❌ Missing required dependencies:"
    for dep in "${MISSING_DEPS[@]}"; do
        echo "  - $dep"
    done
    echo ""
    echo "Installation instructions:"

    if [[ " ${MISSING_DEPS[*]} " =~ " FUSE " ]]; then
        echo ""
        case " $DISTRO_ID $DISTRO_LIKE " in
            *" ubuntu "*|*" debian "*) echo "Ubuntu/Debian:"; echo "  sudo apt install libfuse2t64 fuse  # use libfuse2 where libfuse2t64 is unavailable" ;;
            *" fedora "*) echo "Fedora:"; echo "  sudo dnf install fuse-libs fuse" ;;
            *" arch "*) echo "Arch:"; echo "  sudo pacman -Syu --needed fuse2" ;;
            *" opensuse "*|*" suse "*) echo "openSUSE:"; echo "  sudo zypper install libfuse2 fuse" ;;
            *) echo "Install your distribution's FUSE 2 runtime and fusermount helper." ;;
        esac
    fi
    if printf '%s\n' "${MISSING_DEPS[@]}" | grep -Eq '^(libfribidi\.so\.0|libharfbuzz\.so\.0|libfontconfig\.so\.1|libwayland-client\.so\.0|libfreetype\.so\.6|libX11-xcb\.so\.1|libX11\.so\.6|libpipewire-0\.3\.so\.0|libcom_err\.so\.2|libgpg-error\.so\.0)$'; then
        echo ""
        case " $DISTRO_ID $DISTRO_LIKE " in
            *" ubuntu "*|*" debian "*) echo "Ubuntu/Debian:"; echo "  sudo apt install libfribidi0 libharfbuzz0b libfontconfig1 libwayland-client0 libfreetype6 libx11-xcb1 libx11-6 libpipewire-0.3-0 libcom-err2 libgpg-error0" ;;
            *" fedora "*) echo "Fedora:"; echo "  sudo dnf install fribidi harfbuzz fontconfig libwayland-client freetype libX11-xcb libX11 pipewire-libs libcom_err libgpg-error" ;;
            *" arch "*) echo "Arch:"; echo "  sudo pacman -Syu --needed fribidi harfbuzz fontconfig wayland freetype2 libx11 pipewire e2fsprogs libgpg-error" ;;
            *" opensuse "*|*" suse "*) echo "openSUSE:"; echo "  sudo zypper install libfribidi0 libharfbuzz0 libfontconfig1 libwayland-client0 libfreetype6 libX11-xcb1 libX11-6 libpipewire-0_3-0 libcom_err2 libgpg-error0" ;;
            *) echo "Install the X11, PipeWire, font, and Wayland runtime libraries required by the AppImage." ;;
        esac
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
    echo "  sudo apt install pipewire pipewire-pulse wireplumber pulseaudio-utils"
    echo "  systemctl --user enable --now pipewire wireplumber"
    echo ""
    echo "Fedora:"
    echo "  sudo dnf install pipewire pipewire-utils pipewire-pulseaudio wireplumber pulseaudio-utils"
    echo "  systemctl --user enable --now pipewire wireplumber"
    echo ""
    echo "Arch:"
    echo "  sudo pacman -Syu --needed pipewire pipewire-pulse wireplumber libpulse"
    echo "  systemctl --user enable --now pipewire wireplumber"
    echo ""
fi

exit 0
