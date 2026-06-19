#!/usr/bin/env bash
# Linux Soundboard installer
#
# Detects your distro and installs via the native package manager.
# Falls back to the tarball installer on unsupported distros.
# No root required on the fallback path; sudo is needed for package manager paths.
#
# Usage: bash <(curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/install.sh)
#        curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/install.sh | bash -s -- uninstall --yes

set -euo pipefail

APP_REPO="germanua/Linux-SoundBoard"
APP_PACKAGE="linux-soundboard"
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

usage() {
    cat <<EOF
Linux Soundboard installer

Usage:
  ./install.sh [install]
  ./install.sh repair [binary]
  ./install.sh status
  ./install.sh remove [--yes] [--keep-data] [--keep-package] [--restore-default-source|--keep-current-default-source]
  ./install.sh uninstall [--yes] [--keep-data] [--keep-package] [--restore-default-source|--keep-current-default-source]
  ./install.sh --help

The default install command detects your distro and installs via a native
package when available. The remove/uninstall command removes per-user files and
also removes the native Linux Soundboard package when one is installed. Pass
--keep-package to leave the native package installed.
EOF
}

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
    info "Downloading $url ..." >&2
    fetch_progress "$url" "$tarball"

    info "Extracting..." >&2
    tar -xzf "$tarball" -C "$WORK_DIR"

    find "$WORK_DIR" -mindepth 1 -maxdepth 1 -type d | head -1
}

run_user_installer() {
    local mode=$1   # install | repair | remove | status
    local bundle_dir=$2
    shift 2

    local installer="$bundle_dir/install-user.sh"
    [[ -x "$installer" ]] || chmod +x "$installer"
    "$installer" "$mode" "$@"
}

local_user_installer() {
    local script_path="${BASH_SOURCE[0]:-$0}"
    local script_dir
    local candidate

    if script_dir="$(cd -- "$(dirname -- "$script_path")" >/dev/null 2>&1 && pwd -P)"; then
        :
    else
        script_dir="$(pwd)"
    fi

    for candidate in \
        "$script_dir/packaging/linux/install-user.sh" \
        "$script_dir/install-user.sh"; do
        if [[ -f "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

run_user_installer_from_available_source() {
    local mode=$1
    shift
    local installer
    local bundle_dir

    if installer="$(local_user_installer)"; then
        [[ -x "$installer" ]] || chmod +x "$installer"
        "$installer" "$mode" "$@"
        return
    fi

    bundle_dir="$(download_and_extract_tarball)"
    run_user_installer "$mode" "$bundle_dir" "$@"
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
    local url; url="$(find_asset_url '\\.deb$' || true)"
    if [[ -z "$url" ]]; then
        warn "No .deb in latest release; falling back to tarball install."
        install_tarball; return
    fi

    local file
    file="$WORK_DIR/$(basename "$url")"
    info "Downloading .deb..."
    fetch_progress "$url" "$file"
    apt_install "$file"

    # The package owns the binary, desktop entry, icons, and the systemd user
    # unit. Only enable the engine service for the installing account; do not
    # redeploy those files into ~/.local, which would shadow the package and run
    # a stale binary after a package upgrade. The package's postinst already
    # enables the service for new logins, so a failure here is non-fatal.
    run_user_installer_from_available_source setup-user \
        || warn "Could not configure the user service; it will start on next login."
}

install_fedora() {
    local url; url="$(find_asset_url '\\.rpm$' || true)"
    if [[ -z "$url" ]]; then
        warn "No .rpm in latest release; falling back to tarball install."
        install_tarball; return
    fi

    local file
    file="$WORK_DIR/$(basename "$url")"
    info "Downloading .rpm..."
    fetch_progress "$url" "$file"
    dnf_install "$file"

    # The package owns the binary, desktop entry, icons, and the systemd user
    # unit. Only enable the engine service for the installing account; do not
    # redeploy those files into ~/.local, which would shadow the package and run
    # a stale binary after a package upgrade. The package's postinst already
    # enables the service for new logins, so a failure here is non-fatal.
    run_user_installer_from_available_source setup-user \
        || warn "Could not configure the user service; it will start on next login."
}

install_tarball() {
    local bundle_dir; bundle_dir="$(download_and_extract_tarball)"
    run_user_installer install "$bundle_dir"
}

# ── Repair, status, and removal ───────────────────────────────────────────────

as_root() {
    if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        fail "sudo is required to remove the native package."
    fi
}

installed_native_packages() {
    local found=0
    local pkg

    if command -v dpkg-query >/dev/null 2>&1 \
        && dpkg-query -W -f='${Status}' "$APP_PACKAGE" 2>/dev/null | grep -q "install ok installed"; then
        printf 'deb\t%s\n' "$APP_PACKAGE"
        found=1
    fi

    if command -v rpm >/dev/null 2>&1 && rpm -q "$APP_PACKAGE" >/dev/null 2>&1; then
        printf 'rpm\t%s\n' "$APP_PACKAGE"
        found=1
    fi

    if command -v pacman >/dev/null 2>&1; then
        local seen_pacman=$'\n'
        for pkg in "$APP_AUR_PACKAGE" "$APP_PACKAGE"; do
            local actual_pkg
            actual_pkg="$(pacman -Qq "$pkg" 2>/dev/null || true)"
            if [[ -n "$actual_pkg" && "$seen_pacman" != *$'\n'"$actual_pkg"$'\n'* ]]; then
                printf 'pacman\t%s\n' "$actual_pkg"
                seen_pacman+="$actual_pkg"$'\n'
                found=1
            fi
        done
    fi

    ((found == 1))
}

remove_deb_package() {
    if command -v apt-get >/dev/null 2>&1; then
        as_root apt-get remove -y "$APP_PACKAGE"
    else
        as_root dpkg -r "$APP_PACKAGE"
    fi
}

remove_rpm_package() {
    if command -v dnf >/dev/null 2>&1; then
        as_root dnf remove -y "$APP_PACKAGE"
    elif command -v zypper >/dev/null 2>&1; then
        as_root zypper --non-interactive remove "$APP_PACKAGE"
    else
        as_root rpm -e "$APP_PACKAGE"
    fi
}

remove_pacman_package() {
    local pkg=$1

    as_root pacman -Rns --noconfirm "$pkg"
}

remove_native_packages() {
    local found=0
    local kind
    local pkg

    while IFS=$'\t' read -r kind pkg; do
        [[ -n "${kind:-}" && -n "${pkg:-}" ]] || continue
        found=1
        info "Removing native package: $pkg"
        case "$kind" in
            deb)
                remove_deb_package
                ;;
            rpm)
                remove_rpm_package
                ;;
            pacman)
                remove_pacman_package "$pkg"
                ;;
        esac
    done < <(installed_native_packages || true)

    if ((found == 0)); then
        info "No native Linux Soundboard package is installed."
    fi
}

print_native_package_status() {
    local packages=()
    local kind
    local pkg

    while IFS=$'\t' read -r kind pkg; do
        [[ -n "${kind:-}" && -n "${pkg:-}" ]] || continue
        packages+=("$kind:$pkg")
    done < <(installed_native_packages || true)

    if ((${#packages[@]} == 0)); then
        printf '  Native pkg:    missing\n'
    else
        printf '  Native pkg:    %s\n' "${packages[*]}"
    fi
}

REMOVE_KEEP_PACKAGE=0
USER_REMOVE_ARGS=()

parse_wrapper_remove_args() {
    REMOVE_KEEP_PACKAGE=0
    USER_REMOVE_ARGS=()

    while (($# > 0)); do
        case "$1" in
            --keep-package)
                REMOVE_KEEP_PACKAGE=1
                ;;
            *)
                USER_REMOVE_ARGS+=("$1")
                ;;
        esac
        shift
    done
}

remove_installation() {
    parse_wrapper_remove_args "$@"
    run_user_installer_from_available_source remove "${USER_REMOVE_ARGS[@]}"

    if ((REMOVE_KEEP_PACKAGE == 1)); then
        info "Keeping native package because --keep-package was passed."
    else
        remove_native_packages
    fi
}

print_status() {
    run_user_installer_from_available_source status
    print_native_package_status
}

# ── swhkd (Wayland global hotkeys) ───────────────────────────────────────────

build_swhkd_from_source() {
    local src="$WORK_DIR/swhkd"
    git clone --depth 1 "$SWHKD_REPO_URL" "$src"
    (
        cd "$src"
        make clean 2>/dev/null || true
        make
    )
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

configure_swhkd_permissions() {
    local swhkd_path
    local swhks_path

    swhkd_path="$(command -v swhkd 2>/dev/null || true)"
    swhks_path="$(command -v swhks 2>/dev/null || true)"

    [[ -n "$swhkd_path" ]] || fail "swhkd was not found after installation."
    [[ -n "$swhks_path" ]] || fail "swhks was not found after installation."

    info "Configuring swhkd permissions..."
    sudo chown root:root "$swhkd_path"
    sudo chmod u+s "$swhkd_path"
    sudo chmod +x "$swhks_path"

    [[ -u "$swhkd_path" ]] || fail "swhkd setuid bit was not applied to $swhkd_path."
}

swhkd_requires_pkexec() {
    local swhkd_path
    local swhks_path
    local output_file
    local swhks_pid=""
    local status=0

    swhkd_path="$(command -v swhkd 2>/dev/null || true)"
    swhks_path="$(command -v swhks 2>/dev/null || true)"

    [[ -n "$swhkd_path" && -n "$swhks_path" ]] || return 1

    output_file="$WORK_DIR/swhkd-direct-launch-check.log"
    : > "$output_file"

    "$swhks_path" >>"$output_file" 2>&1 &
    swhks_pid=$!
    sleep 0.3

    set +e
    if command -v timeout >/dev/null 2>&1; then
        timeout 2s "$swhkd_path" >>"$output_file" 2>&1
        status=$?
    else
        "$swhkd_path" >>"$output_file" 2>&1 &
        local swhkd_pid=$!
        sleep 2
        if kill -0 "$swhkd_pid" >/dev/null 2>&1; then
            kill "$swhkd_pid" >/dev/null 2>&1
            wait "$swhkd_pid" >/dev/null 2>&1
            status=124
        else
            wait "$swhkd_pid" >/dev/null 2>&1
            status=$?
        fi
    fi
    set -e

    if [[ -n "$swhks_pid" ]]; then
        kill "$swhks_pid" >/dev/null 2>&1 || true
        wait "$swhks_pid" >/dev/null 2>&1 || true
    fi

    if grep -qiE 'launch the binary with pkexec|failed to launch swhkd' "$output_file"; then
        warn "Installed swhkd refuses direct launch; rebuilding from upstream source."
        return 0
    fi

    if ((status == 124)); then
        info "swhkd direct-launch check stayed running; keeping current binary."
    fi

    return 1
}

install_swhkd() {
    if command -v swhkd >/dev/null 2>&1 && command -v swhks >/dev/null 2>&1; then
        info "swhkd already installed; checking permissions."
        configure_swhkd_permissions
        if ! swhkd_requires_pkexec; then
            return
        fi
    fi

    info "Installing swhkd from upstream source for Wayland hotkeys..."
    case "$DISTRO_FAMILY" in
        arch)
            pacman_install base-devel git make rust cargo pkgconf systemd
            build_swhkd_from_source
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

    configure_swhkd_permissions
}

repair_swhkd_if_needed() {
    if is_wayland; then
        install_swhkd
    elif command -v swhkd >/dev/null 2>&1 && command -v swhks >/dev/null 2>&1; then
        configure_swhkd_permissions
    fi
}

# ── PipeWire services ─────────────────────────────────────────────────────────

ensure_pipewire_services() {
    command -v systemctl >/dev/null 2>&1 || return
    local svc
    for svc in pipewire.service wireplumber.service; do
        if systemctl --user list-unit-files "$svc" >/dev/null 2>&1; then
            systemctl --user enable --now "$svc" >/dev/null 2>&1 || true
        fi
    done
}

# ── Main ──────────────────────────────────────────────────────────────────────

install_main() {
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

repair_main() {
    detect_distro
    detect_session

    # On a native-package install, repair the user service only. A full repair
    # would deploy a ~/.local copy that shadows the package. An explicit binary
    # argument still forces a full repair (source builds).
    if [[ $# -eq 0 ]] && installed_native_packages >/dev/null 2>&1; then
        info "Native Linux Soundboard package detected; configuring the user service only."
        run_user_installer_from_available_source setup-user
    else
        run_user_installer_from_available_source repair "$@"
    fi

    repair_swhkd_if_needed
    ensure_pipewire_services
}

main() {
    local command="${1:-install}"

    case "$command" in
        --help|-h|help)
            usage
            return
            ;;
    esac

    [[ ${EUID:-$(id -u)} -eq 0 ]] && fail "Run as your regular user, not root."

    case "$command" in
        install)
            [[ $# -gt 0 ]] && shift
            install_main "$@"
            ;;
        repair)
            [[ $# -gt 0 ]] && shift
            repair_main "$@"
            ;;
        status)
            [[ $# -gt 0 ]] && shift
            print_status
            ;;
        remove|uninstall)
            [[ $# -gt 0 ]] && shift
            remove_installation "$@"
            ;;
        *)
            usage
            exit 1
            ;;
    esac
}

main "$@"
