#!/usr/bin/env bash
# Bootstrap installer for Linux Soundboard.
# Supported:
# - Arch family: installs linux-soundboard-git from the AUR
# - Debian/Ubuntu family: installs the latest .deb release
# - Fedora family: installs the latest .rpm release
# - openSUSE/SUSE family: installs the latest AppImage and host runtime deps
#
# On Wayland sessions, the script also installs and configures swhkd.

set -euo pipefail

APP_REPO="germanua/Linux-SoundBoard"
APP_AUR_PACKAGE="linux-soundboard-git"
APP_BINARY="linux-soundboard"
APPIMAGE_NAME="linux-soundboard-x86_64.AppImage"
SWHKD_REPO_URL="https://github.com/waycrate/swhkd.git"

SCRIPT_NAME="$(basename "$0")"
WORK_DIR="$(mktemp -d)"
LATEST_RELEASE_JSON=""
APT_UPDATED=0
ZYPPER_REFRESHED=0

trap 'rm -rf "$WORK_DIR"' EXIT

log() {
    printf '[%s] %s\n' "$1" "$2"
}

info() {
    log INFO "$1"
}

warn() {
    log WARN "$1" >&2
}

fail() {
    log ERROR "$1" >&2
    exit 1
}

success() {
    log OK "$1"
}

need_non_root_user() {
    if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
        fail "Run $SCRIPT_NAME as your regular user. It uses sudo only where required."
    fi
}

need_sudo() {
    if ! command -v sudo >/dev/null 2>&1; then
        fail "sudo is required."
    fi

    if ! sudo -v; then
        fail "sudo authentication failed."
    fi
}

require_os_release() {
    if [[ ! -r /etc/os-release ]]; then
        fail "Cannot detect the distribution because /etc/os-release is missing."
    fi

    # shellcheck disable=SC1091
    source /etc/os-release

    DISTRO_ID="${ID:-unknown}"
    DISTRO_NAME="${PRETTY_NAME:-$DISTRO_ID}"
    DISTRO_VERSION="${VERSION_ID:-unknown}"
    DISTRO_LIKE="${ID_LIKE:-}"

    mapfile -t DISTRO_IDS < <(
        {
            printf '%s\n' "$DISTRO_ID"
            printf '%s\n' "$DISTRO_LIKE" | tr ' ' '\n'
        } | tr '[:upper:]' '[:lower:]' | sed '/^$/d' | awk '!seen[$0]++'
    )
}

detect_distro_family() {
    DISTRO_FAMILY="other"

    for distro in "${DISTRO_IDS[@]}"; do
        case "$distro" in
            arch|manjaro|endeavouros)
                DISTRO_FAMILY="arch"
                return
                ;;
            ubuntu|debian|linuxmint|pop|elementary|zorin)
                DISTRO_FAMILY="debian"
                return
                ;;
            fedora|nobara)
                DISTRO_FAMILY="fedora"
                return
                ;;
            opensuse*|sles|suse)
                DISTRO_FAMILY="opensuse"
                return
                ;;
        esac
    done
}

detect_session_type() {
    SESSION_TYPE=""

    if [[ -n "${XDG_SESSION_TYPE:-}" ]]; then
        SESSION_TYPE="${XDG_SESSION_TYPE,,}"
    elif [[ -n "${WAYLAND_DISPLAY:-}" ]]; then
        SESSION_TYPE="wayland"
    elif [[ -n "${DISPLAY:-}" ]]; then
        SESSION_TYPE="x11"
    elif command -v loginctl >/dev/null 2>&1 && [[ -n "${XDG_SESSION_ID:-}" ]]; then
        SESSION_TYPE="$(loginctl show-session "$XDG_SESSION_ID" -p Type --value 2>/dev/null | tr '[:upper:]' '[:lower:]')"
    fi

    if [[ -z "$SESSION_TYPE" ]]; then
        SESSION_TYPE="unknown"
    fi
}

is_wayland_session() {
    [[ "$SESSION_TYPE" == "wayland" ]] || [[ -n "${WAYLAND_DISPLAY:-}" ]]
}

check_architecture() {
    local arch
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64)
            ;;
        *)
            fail "This installer currently supports x86_64 only. Detected architecture: $arch"
            ;;
    esac
}

download_file() {
    local url=$1
    local output=$2

    if command -v curl >/dev/null 2>&1; then
        curl -fL --progress-bar "$url" -o "$output"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --show-progress "$url" -O "$output"
    else
        fail "Neither curl nor wget is installed."
    fi
}

get_latest_release_json() {
    if [[ -z "$LATEST_RELEASE_JSON" ]]; then
        LATEST_RELEASE_JSON="$(curl -fsSL "https://api.github.com/repos/$APP_REPO/releases/latest")"
    fi

    printf '%s' "$LATEST_RELEASE_JSON"
}

find_release_asset_url() {
    local pattern=$1

    get_latest_release_json \
        | grep -oE '"browser_download_url":[[:space:]]*"[^"]+"' \
        | sed -E 's/^"browser_download_url":[[:space:]]*"([^"]+)"$/\1/' \
        | grep -E "$pattern" \
        | head -n 1
}

apt_install() {
    if (( APT_UPDATED == 0 )); then
        sudo apt-get update
        APT_UPDATED=1
    fi

    sudo apt-get install -y "$@"
}

pacman_install() {
    sudo pacman -S --needed --noconfirm "$@"
}

dnf_install() {
    sudo dnf install -y "$@"
}

zypper_refresh() {
    if (( ZYPPER_REFRESHED == 0 )); then
        sudo zypper --non-interactive refresh
        ZYPPER_REFRESHED=1
    fi
}

zypper_install() {
    zypper_refresh
    sudo zypper --non-interactive install --no-recommends "$@"
}

zypper_pick_package() {
    local pkg

    for pkg in "$@"; do
        if sudo zypper --non-interactive info "$pkg" >/dev/null 2>&1; then
            printf '%s\n' "$pkg"
            return 0
        fi
    done

    return 1
}

apt_pick_package() {
    local pkg

    for pkg in "$@"; do
        if apt-cache show "$pkg" >/dev/null 2>&1; then
            printf '%s\n' "$pkg"
            return 0
        fi
    done

    return 1
}

ensure_download_prereqs() {
    case "$DISTRO_FAMILY" in
        arch)
            if ! command -v curl >/dev/null 2>&1; then
                pacman_install curl ca-certificates
            fi
            ;;
        debian)
            if ! command -v curl >/dev/null 2>&1; then
                apt_install curl ca-certificates
            fi
            ;;
        fedora)
            if ! command -v curl >/dev/null 2>&1; then
                dnf_install curl ca-certificates
            fi
            ;;
        opensuse)
            if ! command -v curl >/dev/null 2>&1; then
                zypper_install curl ca-certificates
            fi
            ;;
    esac
}

aur_install_with_helper() {
    local helper=$1
    shift
    "$helper" -S --needed --noconfirm "$@"
}

aur_install_manual() {
    local package_name=$1
    local package_dir="$WORK_DIR/$package_name"

    rm -rf "$package_dir"
    git clone --depth 1 "https://aur.archlinux.org/${package_name}.git" "$package_dir"

    (
        cd "$package_dir"
        makepkg -si --needed --noconfirm
    )
}

aur_install() {
    local package_name=$1

    pacman_install base-devel git

    if command -v yay >/dev/null 2>&1; then
        aur_install_with_helper yay "$package_name"
        return
    fi

    if command -v paru >/dev/null 2>&1; then
        aur_install_with_helper paru "$package_name"
        return
    fi

    aur_install_manual "$package_name"
}

install_app_arch() {
    info "Installing $APP_AUR_PACKAGE from the AUR."
    aur_install "$APP_AUR_PACKAGE"
}

install_app_debian() {
    local url
    local file

    url="$(find_release_asset_url '\.deb$' || true)"
    if [[ -z "$url" ]]; then
        warn "No .deb asset is currently published in GitHub releases. Falling back to AppImage on Debian/Ubuntu."
        install_app_appimage
        return
    fi

    file="$WORK_DIR/$(basename "$url")"
    info "Downloading the latest Debian package."
    download_file "$url" "$file"

    info "Installing the Debian package with dependency resolution."
    apt_install "$file"
}

install_app_fedora() {
    local url
    local file

    url="$(find_release_asset_url '\.rpm$' || true)"
    if [[ -z "$url" ]]; then
        warn "No .rpm asset is currently published in GitHub releases. Falling back to AppImage on Fedora."
        install_app_appimage
        return
    fi

    file="$WORK_DIR/$(basename "$url")"
    info "Downloading the latest RPM package."
    download_file "$url" "$file"

    info "Installing the RPM package with dependency resolution."
    dnf_install "$file"
}

install_app_appimage() {
    local url
    local install_dir="$HOME/.local/bin"
    local appimage_path="$install_dir/$APPIMAGE_NAME"
    local fuse_package=""

    info "Installing the AppImage fallback."

    mkdir -p "$install_dir"

    url="$(find_release_asset_url '\.AppImage$')"
    [[ -n "$url" ]] || fail "Could not locate the latest AppImage asset in GitHub releases."

    case "$DISTRO_FAMILY" in
        debian)
            fuse_package="$(apt_pick_package libfuse2t64 libfuse2 || true)"
            [[ -n "$fuse_package" ]] || fail "Could not locate a FUSE2 userspace package (tried libfuse2t64, libfuse2)."
            apt_install "$fuse_package" pipewire wireplumber
            ;;
        fedora)
            dnf_install fuse-libs pipewire wireplumber
            ;;
        opensuse)
            zypper_install fuse pipewire wireplumber
            ;;
        *)
            warn "Falling back to AppImage without distro-specific runtime dependency automation."
            ;;
    esac

    download_file "$url" "$appimage_path"
    chmod +x "$appimage_path"
    ln -sf "$appimage_path" "$install_dir/$APP_BINARY"

    if [[ ":$PATH:" != *":$install_dir:"* ]]; then
        warn "$install_dir is not in PATH. Add it to your shell profile to launch $APP_BINARY directly."
    fi
}

build_and_install_swhkd_from_source() {
    local source_dir="$WORK_DIR/swhkd"
    local manpage

    rm -rf "$source_dir"
    git clone --depth 1 "$SWHKD_REPO_URL" "$source_dir"

    (
        cd "$source_dir"
        make clean || true
        make
    )

    sudo install -Dm755 "$source_dir/target/release/swhkd" /usr/bin/swhkd
    sudo install -Dm755 "$source_dir/target/release/swhks" /usr/bin/swhks

    for manpage in "$source_dir"/docs/*.gz; do
        [[ -e "$manpage" ]] || continue
        case "$(basename "$manpage")" in
            *.1.gz)
                sudo install -Dm644 "$manpage" "/usr/share/man/man1/$(basename "$manpage")"
                ;;
            *.5.gz)
                sudo install -Dm644 "$manpage" "/usr/share/man/man5/$(basename "$manpage")"
                ;;
        esac
    done

    if [[ ! -f /etc/swhkd/swhkdrc ]]; then
        sudo install -Dm644 /dev/null /etc/swhkd/swhkdrc
    fi
}

install_swhkd_arch() {
    if command -v swhkd >/dev/null 2>&1 && command -v swhks >/dev/null 2>&1; then
        info "swhkd and swhks are already installed."
        return
    fi

    info "Installing swhkd from the AUR for Wayland hotkeys."
    if ! aur_install swhkd-bin; then
        warn "swhkd-bin failed; trying swhkd-git."
        aur_install swhkd-git
    fi
}

install_swhkd_debian() {
    if command -v swhkd >/dev/null 2>&1 && command -v swhks >/dev/null 2>&1; then
        info "swhkd and swhks are already installed."
        return
    fi

    info "Installing swhkd build dependencies from Debian/Ubuntu repositories."
    apt_install git make build-essential pkg-config libudev-dev cargo rustc
    build_and_install_swhkd_from_source
}

install_swhkd_fedora() {
    if command -v swhkd >/dev/null 2>&1 && command -v swhks >/dev/null 2>&1; then
        info "swhkd and swhks are already installed."
        return
    fi

    info "Installing swhkd build dependencies from Fedora repositories."
    dnf_install git make gcc cargo rust pkgconf-pkg-config systemd-devel
    build_and_install_swhkd_from_source
}

install_swhkd_opensuse() {
    local pkg_config_pkg=""
    local udev_dev_pkg=""

    if command -v swhkd >/dev/null 2>&1 && command -v swhks >/dev/null 2>&1; then
        info "swhkd and swhks are already installed."
        return
    fi

    pkg_config_pkg="$(zypper_pick_package pkg-config pkgconf-pkg-config || true)"
    udev_dev_pkg="$(zypper_pick_package systemd-devel libudev-devel || true)"

    [[ -n "$pkg_config_pkg" ]] || fail "Could not locate a pkg-config package in the configured zypper repositories."
    [[ -n "$udev_dev_pkg" ]] || fail "Could not locate a libudev development package in the configured zypper repositories."

    info "Installing swhkd build dependencies from openSUSE repositories."
    zypper_install git make gcc cargo rust "$pkg_config_pkg" "$udev_dev_pkg"
    build_and_install_swhkd_from_source
}

configure_swhkd_permissions() {
    local swhkd_path
    local swhks_path

    swhkd_path="$(command -v swhkd || true)"
    swhks_path="$(command -v swhks || true)"

    [[ -n "$swhkd_path" ]] || fail "swhkd was expected but is still missing from PATH."
    [[ -n "$swhks_path" ]] || fail "swhks was expected but is still missing from PATH."

    sudo chown root:root "$swhkd_path"
    sudo chmod u+s "$swhkd_path"
    sudo chmod +x "$swhks_path"
}

ensure_pipewire_services() {
    local service

    if ! command -v systemctl >/dev/null 2>&1; then
        return
    fi

    for service in pipewire.service wireplumber.service; do
        if systemctl --user list-unit-files "$service" >/dev/null 2>&1; then
            systemctl --user enable --now "$service" >/dev/null 2>&1 || true
        fi
    done
}

verify_installation() {
    if ! command -v "$APP_BINARY" >/dev/null 2>&1; then
        warn "$APP_BINARY is not in PATH yet. If the AppImage path is outside PATH, reopen your shell after adding ~/.local/bin."
    fi

    if is_wayland_session; then
        if command -v swhkd >/dev/null 2>&1 && [[ -u "$(command -v swhkd)" ]]; then
            success "Wayland hotkey support is installed and swhkd has the setuid bit."
        else
            fail "Wayland hotkey setup did not complete correctly."
        fi
    fi

    if pgrep -x pipewire >/dev/null 2>&1; then
        success "PipeWire is running."
    else
        warn "PipeWire is not running yet. You may need to log out and back in after installation."
    fi
}

print_summary() {
    printf '\n'
    printf 'Linux Soundboard bootstrap complete.\n'
    printf '  Distro:  %s\n' "$DISTRO_NAME"
    printf '  Session: %s\n' "$SESSION_TYPE"
    printf '  App:     %s\n' "$(command -v "$APP_BINARY" 2>/dev/null || printf 'not in PATH yet')"

    if is_wayland_session; then
        printf '  swhkd:   %s\n' "$(command -v swhkd 2>/dev/null || printf 'missing')"
    else
        printf '  swhkd:   skipped (session is not Wayland)\n'
    fi

    printf '\n'
    printf 'Launch with: %s\n' "$APP_BINARY"
}

main() {
    need_non_root_user
    need_sudo
    require_os_release
    detect_distro_family
    detect_session_type
    check_architecture

    info "Detected distro: $DISTRO_NAME"
    info "Detected session: $SESSION_TYPE"
    ensure_download_prereqs

    case "$DISTRO_FAMILY" in
        arch)
            install_app_arch
            ;;
        debian)
            install_app_debian
            ;;
        fedora)
            install_app_fedora
            ;;
        opensuse)
            install_app_appimage
            ;;
        *)
            warn "Unsupported distro family '$DISTRO_ID'. Falling back to AppImage only."
            install_app_appimage
            ;;
    esac

    if is_wayland_session; then
        case "$DISTRO_FAMILY" in
            arch)
                install_swhkd_arch
                ;;
            debian)
                install_swhkd_debian
                ;;
            fedora)
                install_swhkd_fedora
                ;;
            opensuse)
                install_swhkd_opensuse
                ;;
            *)
                fail "Wayland was detected, but automatic swhkd installation is only implemented for Arch, Debian/Ubuntu, Fedora, and openSUSE."
                ;;
        esac

        configure_swhkd_permissions
    else
        info "Skipping swhkd installation because this session is not Wayland."
    fi

    ensure_pipewire_services
    verify_installation
    print_summary
}

main "$@"
