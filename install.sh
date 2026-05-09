#!/usr/bin/env bash
# Linux Soundboard installer
#
# Detects your distro and installs via the native package manager.
# Falls back to the tarball installer on unsupported distros.
# No root required on the fallback path; sudo is needed for package manager paths.
#
# Usage: bash <(curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/install.sh)

set -euo pipefail

APP_REPO="germanua/Linux-SoundBoard"
APP_BINARY="linux-soundboard"
APP_AUR_PACKAGE="linux-soundboard-git"
SWHKD_REPO_URL="https://github.com/waycrate/swhkd.git"

WORK_DIR="$(mktemp -d)"
LATEST_RELEASE_JSON=""
APT_UPDATED=0
ZYPPER_REFRESHED=0

trap 'rm -rf "$WORK_DIR"' EXIT

log()     { printf '[%s] %s\n' "$1" "$2"; }
info()    { log INFO "$1"; }
warn()    { log WARN "$1" >&2; }
fail()    { log ERROR "$1" >&2; exit 1; }

# ── Download helpers ──────────────────────────────────────────────────────────

if command -v curl >/dev/null 2>&1; then
    fetch()        { curl -fsSL "$1" -o "$2"; }
    fetch_stdout() { curl -fsSL "$1"; }
    fetch_progress(){ curl -fL --progress-bar "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch()        { wget -qO "$2" "$1"; }
    fetch_stdout() { wget -qO- "$1"; }
    fetch_progress(){ wget -q --show-progress -O "$2" "$1"; }
else
    fail "curl or wget is required."
fi

get_release_json() {
    if [[ -z "$LATEST_RELEASE_JSON" ]]; then
        LATEST_RELEASE_JSON="$(fetch_stdout "https://api.github.com/repos/$APP_REPO/releases/latest")" \
            || fail "Could not reach GitHub API."
    fi
    printf '%s' "$LATEST_RELEASE_JSON"
}

find_asset_url() {
    get_release_json \
        | grep -oE '"browser_download_url":[[:space:]]*"[^"]+"' \
        | sed -E 's/.*"([^"]+)"/\1/' \
        | grep -E "$1" | head -1
}

# ── Distro detection ──────────────────────────────────────────────────────────

detect_distro() {
    [[ -r /etc/os-release ]] || fail "/etc/os-release not found; cannot detect distro."
    # shellcheck disable=SC1091
    source /etc/os-release
    DISTRO_NAME="${PRETTY_NAME:-${ID:-unknown}}"
    DISTRO_FAMILY="other"

    local ids
    mapfile -t ids < <(
        { printf '%s\n' "${ID:-}"; printf '%s\n' "${ID_LIKE:-}" | tr ' ' '\n'; } \
            | tr '[:upper:]' '[:lower:]' | sed '/^$/d' | awk '!seen[$0]++'
    )

    for id in "${ids[@]}"; do
        case "$id" in
            arch|manjaro|endeavouros|cachyos) DISTRO_FAMILY="arch";    return ;;
            ubuntu|debian|linuxmint|pop|elementary|zorin)
                                              DISTRO_FAMILY="debian";  return ;;
            fedora|nobara)                    DISTRO_FAMILY="fedora";  return ;;
            opensuse*|sles|suse)              DISTRO_FAMILY="opensuse";return ;;
        esac
    done
}

detect_session() {
    SESSION_TYPE="${XDG_SESSION_TYPE:-}"
    [[ -z "$SESSION_TYPE" && -n "${WAYLAND_DISPLAY:-}" ]] && SESSION_TYPE="wayland"
    [[ -z "$SESSION_TYPE" && -n "${DISPLAY:-}" ]]         && SESSION_TYPE="x11"
    SESSION_TYPE="${SESSION_TYPE:-unknown}"
}

is_wayland() { [[ "$SESSION_TYPE" == "wayland" ]] || [[ -n "${WAYLAND_DISPLAY:-}" ]]; }

# ── Package manager helpers ───────────────────────────────────────────────────

apt_install() {
    if (( APT_UPDATED == 0 )); then sudo apt-get update; APT_UPDATED=1; fi
    sudo apt-get install -y "$@"
}

pacman_install()  { sudo pacman -S --needed --noconfirm "$@"; }
dnf_install()     { sudo dnf install -y "$@"; }

zypper_refresh() {
    if (( ZYPPER_REFRESHED == 0 )); then sudo zypper --non-interactive refresh; ZYPPER_REFRESHED=1; fi
}
zypper_install() { zypper_refresh; sudo zypper --non-interactive install --no-recommends "$@"; }

pick_pkg() {
    # Pick first available package from a list (checks apt-cache or zypper info)
    local cmd=$1; shift
    local pkg
    for pkg in "$@"; do
        if "$cmd" "$pkg" >/dev/null 2>&1; then printf '%s\n' "$pkg"; return 0; fi
    done
    return 1
}

# ── App installation ──────────────────────────────────────────────────────────

# Download the release tarball into WORK_DIR and return the extracted bundle path.
download_and_extract_tarball() {
    local arch; arch="$(uname -m)"
    local url; url="$(find_asset_url "${arch}\\.tar\\.gz")"
    [[ -n "$url" ]] || fail "No release tarball for $arch. See https://github.com/$APP_REPO/releases"

    local tarball="$WORK_DIR/linux-soundboard.tar.gz"
    info "Downloading $url ..."
    fetch_progress "$url" "$tarball"

    info "Extracting..."
    tar -xzf "$tarball" -C "$WORK_DIR"

    find "$WORK_DIR" -mindepth 1 -maxdepth 1 -type d | head -1
}

run_user_installer() {
    local mode=$1   # install | repair
    local bundle_dir=$2

    local installer="$bundle_dir/install-user.sh"
    [[ -x "$installer" ]] || chmod +x "$installer"
    "$installer" "$mode"
}

# Try to download and run user-space setup (desktop integration) from the
# release tarball.  Non-fatal: package-manager installs already handle this,
# so a missing tarball is not a hard error.
try_repair_user_install() {
    local bundle_dir
    if bundle_dir="$(download_and_extract_tarball 2>/dev/null)"; then
        run_user_installer repair "$bundle_dir"
    else
        info "No release tarball found; skipping user-space desktop integration."
    fi
}


install_arch() {
    info "Installing from AUR: $APP_AUR_PACKAGE"
    pacman_install base-devel git

    if command -v yay  >/dev/null 2>&1; then yay  -S --needed --noconfirm "$APP_AUR_PACKAGE"; return; fi
    if command -v paru >/dev/null 2>&1; then paru -S --needed --noconfirm "$APP_AUR_PACKAGE"; return; fi

    # No AUR helper — build manually
    local pkg_dir="$WORK_DIR/$APP_AUR_PACKAGE"
    git clone --depth 1 "https://aur.archlinux.org/${APP_AUR_PACKAGE}.git" "$pkg_dir"
    (cd "$pkg_dir" && makepkg -si --needed --noconfirm)
}

install_debian() {
    local url; url="$(find_asset_url "\\.deb$" || true)"
    if [[ -z "$url" ]]; then
        warn "No .deb in latest release; falling back to tarball install."
        install_tarball; return
    fi

    local file="$WORK_DIR/$(basename "$url")"
    info "Downloading .deb..."
    fetch_progress "$url" "$file"
    apt_install "$file"

    # Run user-space setup (service, PipeWire config) for the installing account.
    try_repair_user_install
}

install_fedora() {
    local url; url="$(find_asset_url "\\.rpm$" || true)"
    if [[ -z "$url" ]]; then
        warn "No .rpm in latest release; falling back to tarball install."
        install_tarball; return
    fi

    local file="$WORK_DIR/$(basename "$url")"
    info "Downloading .rpm..."
    fetch_progress "$url" "$file"
    dnf_install "$file"

    # Run user-space setup (service, PipeWire config) for the installing account.
    try_repair_user_install
}

install_tarball() {
    local bundle_dir; bundle_dir="$(download_and_extract_tarball)"
    run_user_installer install "$bundle_dir"
}

# ── swhkd (Wayland global hotkeys) ───────────────────────────────────────────

build_swhkd_from_source() {
    local src="$WORK_DIR/swhkd"
    git clone --depth 1 "$SWHKD_REPO_URL" "$src"
    (cd "$src" && make clean 2>/dev/null || true; make)
    sudo install -Dm755 "$src/target/release/swhkd" /usr/bin/swhkd
    sudo install -Dm755 "$src/target/release/swhks" /usr/bin/swhks
    for f in "$src"/docs/*.gz; do
        [[ -e "$f" ]] || continue
        case "$(basename "$f")" in
            *.1.gz) sudo install -Dm644 "$f" "/usr/share/man/man1/$(basename "$f")" ;;
            *.5.gz) sudo install -Dm644 "$f" "/usr/share/man/man5/$(basename "$f")" ;;
        esac
    done
    [[ -f /etc/swhkd/swhkdrc ]] || sudo install -Dm644 /dev/null /etc/swhkd/swhkdrc
}

install_swhkd() {
    command -v swhkd >/dev/null 2>&1 && command -v swhks >/dev/null 2>&1 && {
        info "swhkd already installed."; return
    }

    info "Installing swhkd for Wayland hotkeys..."
    case "$DISTRO_FAMILY" in
        arch)
            if ! { aur_try() { command -v "$1" >/dev/null 2>&1 && "$1" -S --needed --noconfirm swhkd-bin; }
                   aur_try yay || aur_try paru; }; then
                pacman_install base-devel git
                build_swhkd_from_source
            fi
            ;;
        debian)
            apt_install git make build-essential pkg-config libudev-dev cargo rustc
            build_swhkd_from_source
            ;;
        fedora)
            dnf_install git make gcc cargo rust pkgconf-pkg-config systemd-devel
            build_swhkd_from_source
            ;;
        opensuse)
            local pkgcfg; pkgcfg="$(pick_pkg "zypper --non-interactive info" pkg-config pkgconf-pkg-config || true)"
            local udevdev; udevdev="$(pick_pkg "zypper --non-interactive info" systemd-devel libudev-devel || true)"
            [[ -n "$pkgcfg" && -n "$udevdev" ]] || fail "Could not locate pkg-config or libudev-devel in zypper repos."
            zypper_install git make gcc cargo rust "$pkgcfg" "$udevdev"
            build_swhkd_from_source
            ;;
        *)
            warn "Wayland detected but automatic swhkd install is not supported on this distro. Use the in-app installer."
            return
            ;;
    esac

    # setuid so swhkd can read /dev/input without root
    local swhkd_path; swhkd_path="$(command -v swhkd)"
    sudo chown root:root "$swhkd_path"
    sudo chmod u+s "$swhkd_path"
}

# ── PipeWire services ─────────────────────────────────────────────────────────

ensure_pipewire_services() {
    command -v systemctl >/dev/null 2>&1 || return
    local svc
    for svc in pipewire.service wireplumber.service; do
        systemctl --user list-unit-files "$svc" >/dev/null 2>&1 \
            && systemctl --user enable --now "$svc" >/dev/null 2>&1 || true
    done
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
    [[ ${EUID:-$(id -u)} -eq 0 ]] && fail "Run as your regular user, not root."

    detect_distro
    detect_session
    info "Distro:  $DISTRO_NAME"
    info "Session: $SESSION_TYPE"

    case "$DISTRO_FAMILY" in
        arch)    install_arch    ;;
        debian)  install_debian  ;;
        fedora)  install_fedora  ;;
        *)       install_tarball ;;
    esac

    if is_wayland; then
        install_swhkd
    fi

    ensure_pipewire_services

    printf '\nDone. Launch with: %s\n' "$APP_BINARY"
}

main "$@"
