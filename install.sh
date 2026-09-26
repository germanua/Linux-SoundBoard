#!/usr/bin/env bash

set -euo pipefail

APP_REPO="germanua/Linux-SoundBoard"
APP_PACKAGE="linux-soundboard"
APP_BINARY="linux-soundboard"
APP_AUR_PACKAGE="linux-soundboard"
APP_AUR_LEGACY_PACKAGE="linux-soundboard-git"
MINISIGN_PUBLIC_KEY="RWTEtl8HnYs8Fg7BOmAXxuC9PUxqlamX5+C0w4FgUUxXGB6DipbZl8tY"
MINISIGN_BOOTSTRAP_URL="https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-linux.tar.gz"
MINISIGN_BOOTSTRAP_SHA256="9a599b48ba6eb7b1e80f12f36b94ceca7c00b7a5173c95c3efc88d9822957e73"
SWHKD_MANAGED_DIR="/usr/local/libexec/linux-soundboard"
SWHKD_MANAGED_BIN="$SWHKD_MANAGED_DIR/swhkd"
SWHKS_MANAGED_BIN="$SWHKD_MANAGED_DIR/swhks"
SWHKD_MANAGED_MARKER="$SWHKD_MANAGED_DIR/.managed-by-linux-soundboard"
SWHKD_TRUSTED_DIR="/usr/libexec/linux-soundboard"
SWHKD_TRUSTED_HELPER="$SWHKD_TRUSTED_DIR/install-swhkd-helper.sh"
SWHKD_TRUSTED_BUILD_SCRIPT="$SWHKD_TRUSTED_DIR/build-swhkd-locked.sh"
SWHKD_TRUSTED_PINNED_LOCK="$SWHKD_TRUSTED_DIR/swhkd-Cargo.lock.pinned"
SWHKD_TRUSTED_MARKER="$SWHKD_TRUSTED_DIR/.managed-by-linux-soundboard"
ISSUE_URL="https://github.com/$APP_REPO/issues/new"

XDG_STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
INSTALL_ROOT="${INSTALL_ROOT:-$HOME/.local/opt/$APP_BINARY}"
INSTALL_VERSION_FILE="$INSTALL_ROOT/.installed-version"

WORK_DIR="$(mktemp -d)"
LATEST_RELEASE_JSON=""
INSTALL_METHOD="auto"
RELEASE_LIST_JSON=""
NATIVE_PACKAGE_PRESENT=0
APT_UPDATED=0
ZYPPER_REFRESHED=0
VERIFIED_ASSET_SHA256=""
DOWNLOADED_APPIMAGE=""

trap 'rm -rf "$WORK_DIR"' EXIT
log()     { printf '[%s] %s\n' "$1" "$2"; }
info()    { log INFO "$1"; }
warn()    { log WARN "$1" >&2; }
fail()    { log ERROR "$1" >&2; exit 1; }
usage() {
    cat <<EOF
Linux Soundboard installer

Usage:
  ./install.sh                 open the menu (needs a terminal)
  ./install.sh menu
  ./install.sh install [--method auto|appimage]
  ./install.sh install --version vX.Y.Z [--method auto|appimage]
  ./install.sh versions
  ./install.sh verify FILE [--version vX.Y.Z]
  ./install.sh repair [binary]
  ./install.sh fix
  ./install.sh report [--output PATH]
  ./install.sh status
  ./install.sh remove [--yes] [--keep-data] [--keep-package] [--restore-default-source|--keep-current-default-source]
  ./install.sh uninstall [--yes] [--keep-data] [--keep-package] [--restore-default-source|--keep-current-default-source]
  ./install.sh --help

With no arguments and a terminal available, the menu opens; piped with no
arguments it installs the newest version, which keeps scripted use working.

install            installs the current signed AppImage into ~/.local
--method           auto (default) and appimage both use the AppImage for current releases
                   (tarball/native remain accepted only for historical compatibility)
install --version  installs that published release from its AppImage by default
verify             verifies a manually downloaded release file without installing it
fix                repairs the install step by step and prints what failed
report             writes a bug report file with system state, app state, and a blank to fill in
remove/uninstall   removes per-user files and the native package unless --keep-package,
                   showing what changed in your audio setup before offering to restore it
EOF
}

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
require_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "$1 is required${2:+ $2}, and it is not installed."
}
normalize_release_version() {
    local version="${1#v}"
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "Invalid release version: $1"
    printf '%s\n' "$version"
}
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$1" | awk '{print $NF}'
    else
        return 1
    fi
}
minisign_verifier() {
    if command -v minisign >/dev/null 2>&1; then
        command -v minisign
        return 0
    fi

    local arch archive actual verifier
    case "$(uname -m)" in
        x86_64|amd64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) warn "No trusted Minisign verifier is available for $(uname -m)."; return 1 ;;
    esac

    archive="$WORK_DIR/minisign-0.12-linux.tar.gz"
    verifier="$WORK_DIR/minisign-linux/$arch/minisign"
    info "Downloading the pinned Minisign verifier..." >&2
    if ! fetch "$MINISIGN_BOOTSTRAP_URL" "$archive" 2>/dev/null; then
        warn "Could not download the pinned Minisign verifier."
        return 1
    fi
    if ! actual="$(sha256_of "$archive")"; then
        warn "No SHA-256 tool found for the Minisign verifier bootstrap."
        return 1
    fi
    if [[ "${actual,,}" != "$MINISIGN_BOOTSTRAP_SHA256" ]]; then
        warn "Minisign verifier checksum mismatch."
        return 1
    fi
    if ! tar -xzf "$archive" -C "$WORK_DIR" "minisign-linux/$arch/minisign" 2>/dev/null; then
        warn "Could not unpack the pinned Minisign verifier."
        return 1
    fi
    chmod 700 "$verifier"
    printf '%s\n' "$verifier"
}
verify_download() {
    local file=$1
    local tag=${2:-}
    local sums="$WORK_DIR/SHA256SUMS.txt"
    local signature="$WORK_DIR/SHA256SUMS.txt.minisig"
    local release_json release_tag sums_url signature_url verifier trusted_comment expected actual

    if [[ -n "$tag" ]]; then
        release_json="$(release_json_for_tag "$tag")"
    else
        release_json="$(get_release_json)"
    fi

    release_tag="$(printf '%s' "$release_json" | release_tag_in || true)"
    [[ -n "$release_tag" ]] || fail "Could not identify the release tag; refusing to verify $(basename "$file")."
    if [[ -n "$tag" && "$release_tag" != "$tag" ]]; then
        fail "GitHub returned release $release_tag while $tag was requested; refusing to continue."
    fi

    sums_url="$(printf '%s' "$release_json" | find_asset_url_in "SHA256SUMS\\.txt$" || true)"
    if [[ -z "$sums_url" ]] || ! fetch "$sums_url" "$sums" 2>/dev/null; then
        fail "Could not download SHA256SUMS.txt; refusing to install unverified $(basename "$file")."
    fi

    signature_url="$(printf '%s' "$release_json" | find_asset_url_in "SHA256SUMS\\.txt\\.minisig$" || true)"
    if [[ -z "$signature_url" ]] || ! fetch "$signature_url" "$signature" 2>/dev/null; then
        fail "Could not download SHA256SUMS.txt.minisig; refusing to trust the checksum manifest."
    fi

    verifier="$(minisign_verifier)" \
        || fail "Could not obtain a trusted Minisign verifier; download aborted."
    if ! trusted_comment="$("$verifier" -V -H -Q -P "$MINISIGN_PUBLIC_KEY" \
        -m "$sums" -x "$signature" 2>/dev/null)"; then
        fail "Invalid signature for SHA256SUMS.txt; download aborted."
    fi
    [[ "$trusted_comment" == "Linux Soundboard release $release_tag" ]] \
        || fail "Checksum signature does not match release $release_tag; download aborted."

    expected="$(awk -v name="$(basename "$file")" '$2 == name || $2 == "*" name { print $1; exit }' "$sums")"
    if [[ -z "$expected" ]]; then
        fail "$(basename "$file") is not listed in SHA256SUMS.txt; refusing to install it."
    fi
    [[ "$expected" =~ ^[0-9a-fA-F]{64}$ ]] \
        || fail "Invalid SHA-256 for $(basename "$file") in SHA256SUMS.txt; refusing to install it."

    if ! actual="$(sha256_of "$file")"; then
        fail "No SHA-256 tool found; install sha256sum, shasum, or openssl."
    fi

    [[ "${actual,,}" == "${expected,,}" ]] \
        || fail "Checksum mismatch for $(basename "$file"). Expected $expected, got $actual. Download aborted."

    VERIFIED_ASSET_SHA256="${expected,,}"
    info "Signature and checksum verified."
}
verify_local_download() {
    local file=""
    local tag=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)
                [[ -n "${2:-}" ]] || fail "--version needs a tag, for example v2.4.3."
                tag="$2"; shift 2
                ;;
            --version=*)
                tag="${1#--version=}"; shift
                ;;
            --*)
                fail "Unknown verify option: $1. See --help."
                ;;
            *)
                [[ -z "$file" ]] || fail "verify accepts one file."
                file="$1"; shift
                ;;
        esac
    done

    [[ -n "$file" ]] || fail "verify needs a downloaded release file."
    [[ -f "$file" ]] || fail "File not found: $file"
    verify_download "$file" "$tag"
    info "Verified $(basename "$file")."
}
get_release_json() {
    if [[ -z "$LATEST_RELEASE_JSON" ]]; then
        LATEST_RELEASE_JSON="$(fetch_stdout "https://api.github.com/repos/$APP_REPO/releases/latest")" \
            || fail "Could not reach GitHub API."
    fi
    printf '%s' "$LATEST_RELEASE_JSON"
}
get_release_list_json() {
    if [[ -z "$RELEASE_LIST_JSON" ]]; then
        RELEASE_LIST_JSON="$(fetch_stdout "https://api.github.com/repos/$APP_REPO/releases?per_page=30")" \
            || fail "Could not reach GitHub API. If this repeats, you may have hit the hourly rate limit; see https://github.com/$APP_REPO/releases"
    fi
    printf '%s' "$RELEASE_LIST_JSON"
}
release_json_for_tag() {
    fetch_stdout "https://api.github.com/repos/$APP_REPO/releases/tags/$1" \
        || fail "No release found for $1. See https://github.com/$APP_REPO/releases"
}
list_release_tags() {
    get_release_list_json \
        | grep -oE '"tag_name":[[:space:]]*"[^"]+"' \
        | sed -E 's/.*"([^"]+)"/\1/'
}
release_tag_in() {
    grep -oE '"tag_name":[[:space:]]*"[^"]+"' \
        | head -1 | sed -E 's/.*"([^"]+)"/\1/'
}
find_asset_url_in() {
    grep -oE '"browser_download_url":[[:space:]]*"[^"]+"' \
        | sed -E 's/.*"([^"]+)"/\1/' \
        | grep -E "$1" | head -1
}
find_asset_url() {
    get_release_json | find_asset_url_in "$1"
}
installed_version() {
    [[ -r "$INSTALL_VERSION_FILE" ]] || return 1
    head -n 1 "$INSTALL_VERSION_FILE"
}
detect_distro() {
    [[ -r /etc/os-release ]] || fail "/etc/os-release not found; cannot detect distro."

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
is_wayland() { [[ "${SESSION_TYPE:-}" == "wayland" ]] || [[ -n "${WAYLAND_DISPLAY:-}" ]]; }
apt_install() {
    if (( APT_UPDATED == 0 )); then as_root apt-get update; APT_UPDATED=1; fi
    as_root apt-get install -y "$@"
}
arch_require_packages() {
    local missing=() pkg
    command -v pacman >/dev/null 2>&1 || fail "pacman is required on Arch-family systems."
    for pkg in "$@"; do
        pacman -Qq "$pkg" >/dev/null 2>&1 || missing+=("$pkg")
    done
    if ((${#missing[@]} > 0)); then
        fail "Arch does not support partial upgrades. Run: sudo pacman -Syu --needed ${missing[*]}; then retry."
    fi
}
dnf_install()     { as_root dnf install -y "$@"; }
zypper_refresh() {
    if (( ZYPPER_REFRESHED == 0 )); then as_root zypper --non-interactive refresh; ZYPPER_REFRESHED=1; fi
}
zypper_install() { zypper_refresh; as_root zypper --non-interactive install --no-recommends "$@"; }
package_available() {
    case "$DISTRO_FAMILY" in
        debian)   apt-cache show "$1" >/dev/null 2>&1 ;;
        opensuse) zypper --non-interactive info "$1" >/dev/null 2>&1 ;;
        *)        return 1 ;;
    esac
}
pick_pkg() {
    local pkg
    for pkg in "$@"; do
        if package_available "$pkg"; then printf '%s\n' "$pkg"; return 0; fi
    done
    return 1
}
download_and_extract_tarball() {
    local tag=${1:-}
    local arch; arch="$(uname -m)"
    local url

    if [[ -n "$tag" ]]; then
        url="$(release_json_for_tag "$tag" | find_asset_url_in "${arch}\\.tar\\.gz")"
    else
        url="$(find_asset_url "${arch}\\.tar\\.gz")"
    fi
    [[ -n "$url" ]] || fail "No release tarball for $arch${tag:+ at $tag}. See https://github.com/$APP_REPO/releases"

    require_cmd tar "to unpack the release tarball"

    local tarball
    tarball="$WORK_DIR/$(basename "$url")"
    info "Downloading $url ..." >&2
    fetch_progress "$url" "$tarball"
    verify_download "$tarball" "$tag" >&2

    info "Extracting..." >&2
    tar -xzf "$tarball" -C "$WORK_DIR"

    find "$WORK_DIR" -mindepth 1 -maxdepth 1 -type d | head -1
}
run_user_installer() {
    local mode=$1
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
        "$script_dir/install-user.sh" \
        "$INSTALL_ROOT/install-user.sh"; do
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

    if installer="$(local_user_installer)"; then
        [[ -x "$installer" ]] || chmod +x "$installer"
        "$installer" "$mode" "$@"
        return
    fi

    local image extract_dir
    download_appimage "$(installed_version || true)" >/dev/null
    image="$DOWNLOADED_APPIMAGE"
    extract_dir="$WORK_DIR/lifecycle-appimage"
    mkdir -p "$extract_dir"
    ( cd "$extract_dir" && "$image" --appimage-extract >/dev/null ) \
        || fail "Could not unpack the verified AppImage for $mode."
    installer="$extract_dir/squashfs-root/usr/libexec/$APP_BINARY/installer/install-user.sh"
    [[ -f "$installer" ]] || fail "The verified AppImage carries no lifecycle installer."
    [[ -x "$installer" ]] || chmod +x "$installer"
    "$installer" "$mode" "$@"
}
install_arch() {
    info "Installing from AUR: $APP_AUR_PACKAGE"
    arch_require_packages base-devel git

    if command -v yay  >/dev/null 2>&1; then yay  -S --needed --noconfirm --useask "$APP_AUR_PACKAGE"; return; fi
    if command -v paru >/dev/null 2>&1; then paru -S --needed --noconfirm --useask "$APP_AUR_PACKAGE"; return; fi

    local pkg_dir="$WORK_DIR/$APP_AUR_PACKAGE"
    local package_file
    git clone --depth 1 "https://aur.archlinux.org/${APP_AUR_PACKAGE}.git" "$pkg_dir"
    (cd "$pkg_dir" && makepkg -s --needed --noconfirm)
    package_file="$(cd "$pkg_dir" && makepkg --packagelist)"
    [[ -f "$package_file" ]] || fail "AUR build did not produce the expected package."
    as_root pacman -U --needed --noconfirm --ask=4 "$package_file"
}
install_debian() {
    local url; url="$(find_asset_url "\\.deb\$" || true)"
    if [[ -z "$url" ]]; then
        warn "No .deb in latest release; falling back to tarball install."
        install_tarball; return
    fi

    local file
    file="$WORK_DIR/$(basename "$url")"
    info "Downloading .deb..."
    fetch_progress "$url" "$file"
    verify_download "$file"
    apt_install "$file"

    run_user_installer_from_available_source setup-user \
        || warn "Could not configure the user service; it will start on next login."
}
install_fedora() {
    local url; url="$(find_asset_url "\\.rpm\$" || true)"
    if [[ -z "$url" ]]; then
        warn "No .rpm in latest release; falling back to tarball install."
        install_tarball; return
    fi

    local file
    file="$WORK_DIR/$(basename "$url")"
    info "Downloading .rpm..."
    fetch_progress "$url" "$file"
    verify_download "$file"
    dnf_install "$file"

    run_user_installer_from_available_source setup-user \
        || warn "Could not configure the user service; it will start on next login."
}
missing_runtime_dependencies() {
    local cache=""
    local cmd
    local lib
    local missing=()

    if command -v ldconfig >/dev/null 2>&1; then
        cache="$(ldconfig -p 2>/dev/null || true)"
    elif [[ -x /sbin/ldconfig ]]; then
        cache="$(/sbin/ldconfig -p 2>/dev/null || true)"
    fi
    if [[ -n "$cache" ]]; then
        for lib in libgtk-4.so.1 libadwaita-1.so.0 libpulse.so.0 libopus.so.0 libpipewire-0.3.so.0 libX11.so.6 libXi.so.6; do
            grep -qF "$lib" <<<"$cache" || missing+=("$lib")
        done
    fi

    for cmd in pactl pw-cli pw-dump pw-metadata wpctl; do
        command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
    done

    ((${#missing[@]} > 0)) && printf '%s\n' "${missing[@]}"
    return 0
}
runtime_packages() {
    local polkit
    case "$DISTRO_FAMILY" in
        arch)
            printf '%s\n' gtk4 libadwaita libpulse opus libx11 libxi hicolor-icon-theme polkit pipewire pipewire-pulse wireplumber
            ;;
        debian)
            polkit="$(pick_pkg pkexec policykit-1 polkitd || true)"
            printf '%s\n' libgtk-4-1 libadwaita-1-0 libpulse0 libopus0 libx11-6 libxi6 pulseaudio-utils pipewire pipewire-pulse wireplumber ${polkit:+"$polkit"}
            ;;
        fedora)
            printf '%s\n' gtk4 libadwaita pulseaudio-libs opus libX11 libXi polkit pulseaudio-utils pipewire pipewire-utils pipewire-pulseaudio wireplumber
            ;;
        opensuse)
            polkit="$(pick_pkg polkit polkit-default-privs || true)"
            printf '%s\n' libgtk-4-1 libadwaita-1-0 libpulse0 libopus0 libX11-6 libXi6 pulseaudio-utils pipewire pipewire-tools pipewire-pulseaudio wireplumber ${polkit:+"$polkit"}
            ;;
    esac
}
ensure_runtime_dependencies() {
    local missing=()
    local pkgs=()

    mapfile -t missing < <(missing_runtime_dependencies)
    ((${#missing[@]} > 0)) || return 0

    warn "The binary needs libraries or commands this system does not have: ${missing[*]}"

    mapfile -t pkgs < <(runtime_packages)
    if ((${#pkgs[@]} == 0)); then
        fail "Install your distro's GTK 4, libadwaita, PulseAudio, Opus, PipeWire, and X11 runtime packages, then run this again."
    fi

    if [[ ! -t 0 ]]; then
        fail "Install these packages and run this again: ${pkgs[*]}"
    fi

    if ! confirm "Install them now (${pkgs[*]})?"; then
        fail "Nothing was installed. The app needs those dependencies."
    fi

    case "$DISTRO_FAMILY" in
        arch)     arch_require_packages "${pkgs[@]}" ;;
        debian)   apt_install    "${pkgs[@]}" ;;
        fedora)   dnf_install    "${pkgs[@]}" ;;
        opensuse) zypper_install "${pkgs[@]}" ;;
    esac

    mapfile -t missing < <(missing_runtime_dependencies)
    ((${#missing[@]} == 0)) || fail "Dependencies are still missing after installation: ${missing[*]}"
}
install_tarball() {
    local bundle_dir

    ensure_runtime_dependencies
    bundle_dir="$(download_and_extract_tarball)"
    run_user_installer install "$bundle_dir"
}
download_appimage() {
    local tag=${1:-}
    local arch; arch="$(uname -m)"
    local url

    if [[ -n "$tag" ]]; then
        url="$(release_json_for_tag "$tag" | find_asset_url_in "${arch}\\.[aA]pp[iI]mage$")"
    else
        url="$(find_asset_url "${arch}\\.[aA]pp[iI]mage$")"
    fi
    [[ -n "$url" ]] || fail "No release AppImage for $arch${tag:+ at $tag}. See https://github.com/$APP_REPO/releases"

    local image
    image="$WORK_DIR/$(basename "$url")"
    info "Downloading $url ..." >&2
    fetch_progress "$url" "$image"
    verify_download "$image" "$tag" >&2
    chmod +x "$image"
    DOWNLOADED_APPIMAGE="$image"

    printf '%s\n' "$image"
}
appimage_glibc_version() {
    local value
    value="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
    value="${value#glibc }"
    [[ "$value" =~ ^([0-9]+)\.([0-9]+) ]] || return 1
    printf '%s.%s
' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
}
ensure_appimage_host_compatibility() {
    local arch version major minor
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64) ;;
        *) fail "Current AppImage releases support x86_64 only; this system is $arch." ;;
    esac
    version="$(appimage_glibc_version || true)"
    [[ -n "$version" ]] || fail "Current AppImage releases require glibc 2.39 or newer."
    major="${version%%.*}"
    minor="${version#*.}"
    if (( major < 2 || (major == 2 && minor < 39) )); then
        fail "Current AppImage releases require glibc 2.39 or newer; this system has glibc $version."
    fi
}
appimage_library_available() {
    local library=$1 cache=""
    if command -v ldconfig >/dev/null 2>&1; then
        cache="$(ldconfig -p 2>/dev/null || true)"
    elif [[ -x /sbin/ldconfig ]]; then
        cache="$(/sbin/ldconfig -p 2>/dev/null || true)"
    fi
    [[ -n "$cache" ]] && grep -qF "$library" <<<"$cache"
}
missing_appimage_host_libraries() {
    local library
    for library in libfribidi.so.0 libharfbuzz.so.0 libfontconfig.so.1 libwayland-client.so.0 libfreetype.so.6 libX11-xcb.so.1 libX11.so.6 libpipewire-0.3.so.0 libcom_err.so.2 libgpg-error.so.0; do
        appimage_library_available "$library" || printf '%s\n' "$library"
    done
}
appimage_host_packages() {
    case "$DISTRO_FAMILY" in
        arch) printf '%s\n' fribidi harfbuzz fontconfig wayland freetype2 libx11 pipewire e2fsprogs libgpg-error ;;
        debian) printf '%s\n' libfribidi0 libharfbuzz0b libfontconfig1 libwayland-client0 libfreetype6 libx11-xcb1 libx11-6 libpipewire-0.3-0 libcom-err2 libgpg-error0 ;;
        fedora) printf '%s\n' fribidi harfbuzz fontconfig libwayland-client freetype libX11-xcb libX11 pipewire-libs libcom_err libgpg-error ;;
        opensuse) printf '%s\n' libfribidi0 libharfbuzz0 libfontconfig1 libwayland-client0 libfreetype6 libX11-xcb1 libX11-6 libpipewire-0_3-0 libcom_err2 libgpg-error0 ;;
    esac
}
ensure_appimage_host_libraries() {
    local missing=() pkgs=()
    mapfile -t missing < <(missing_appimage_host_libraries)
    ((${#missing[@]} == 0)) && return 0
    mapfile -t pkgs < <(appimage_host_packages)
    ((${#pkgs[@]} > 0)) || fail "Current AppImage needs host libraries ${missing[*]}; install your distribution's X11 and PipeWire runtime libraries."
    if [[ ! -t 0 ]]; then
        fail "Current AppImage needs host libraries ${missing[*]}. Install these packages and retry: ${pkgs[*]}"
    fi
    if ! confirm "Install required AppImage host libraries now (${pkgs[*]})?"; then
        fail "Current AppImage needs host libraries ${missing[*]}. Install these packages and retry: ${pkgs[*]}"
    fi
    case "$DISTRO_FAMILY" in
        arch) arch_require_packages "${pkgs[@]}" ;;
        debian) apt_install "${pkgs[@]}" ;;
        fedora) dnf_install "${pkgs[@]}" ;;
        opensuse) zypper_install "${pkgs[@]}" ;;
    esac
    mapfile -t missing < <(missing_appimage_host_libraries)
    ((${#missing[@]} == 0)) || fail "Required AppImage host libraries are still missing: ${missing[*]}"
}
ensure_fuse_for_appimage() {
    command -v fusermount3 >/dev/null 2>&1 && return 0
    command -v fusermount  >/dev/null 2>&1 && return 0

    local pkgs=()
    mapfile -t pkgs < <(fuse_packages)

    warn "FUSE is missing; an installed AppImage cannot start without it."
    if ((${#pkgs[@]} == 0)); then
        warn "Install your distro's FUSE 2 package, then launch the app again."
        return 0
    fi

    if [[ -t 0 ]] && confirm "Install ${pkgs[*]} now?"; then
        case "$DISTRO_FAMILY" in
            arch)     arch_require_packages "${pkgs[@]}" ;;
            debian)   apt_install    "${pkgs[@]}" ;;
            fedora)   dnf_install    "${pkgs[@]}" ;;
            opensuse) zypper_install "${pkgs[@]}" ;;
        esac
    else
        warn "Continuing without FUSE. Install ${pkgs[*]} before launching the app."
    fi
}
fuse_packages() {
    case "$DISTRO_FAMILY" in
        arch)
            printf '%s\n' fuse2
            ;;
        debian)
            pick_pkg libfuse2t64 libfuse2 || true
            pick_pkg fuse || true
            ;;
        fedora)
            printf '%s\n' fuse-libs fuse
            ;;
        opensuse)
            pick_pkg libfuse2 || true
            pick_pkg fuse || true
            ;;
    esac
}
root_sha256_of() {
    local output
    if output="$(as_root sha256sum "$1" 2>/dev/null)"; then
        awk '{print $1}' <<<"$output"
    elif output="$(as_root shasum -a 256 "$1" 2>/dev/null)"; then
        awk '{print $1}' <<<"$output"
    elif output="$(as_root openssl dgst -sha256 "$1" 2>/dev/null)"; then
        awk '{print $NF}' <<<"$output"
    else
        return 1
    fi
}

provision_trusted_swhkd_helper_from_verified_appimage() (
    set -euo pipefail
    local image=$1
    local expected=${2:-}
    local root_dir root_image root_hash source_dir
    [[ "$expected" =~ ^[0-9a-fA-F]{64}$ ]] \
        || fail "The verified release checksum is unavailable; refusing privileged helper provisioning."
    root_dir="$(as_root mktemp -d /var/tmp/linux-soundboard-provision.XXXXXX)"
    trap 'as_root rm -rf -- "$root_dir"' EXIT
    root_image="$root_dir/release.AppImage"

    as_root install -o root -g root -m700 "$image" "$root_image"
    root_hash="$(root_sha256_of "$root_image")" \
        || fail "No root-side SHA-256 tool is available."
    [[ "${root_hash,,}" == "$expected" ]] \
        || fail "The root-owned AppImage copy no longer matches the signed release checksum."

    as_root env -C "$root_dir" ./release.AppImage --appimage-extract >/dev/null
    source_dir="$root_dir/squashfs-root/usr/libexec/$APP_BINARY"
    for file in install-swhkd-helper.sh build-swhkd-locked.sh swhkd-Cargo.lock.pinned; do
        as_root test -f "$source_dir/$file" || fail "Verified AppImage is missing $file."
        as_root test ! -L "$source_dir/$file" || fail "Verified AppImage contains a symlinked $file."
    done

    local trusted_parent trusted_grandparent
    trusted_parent="$(dirname -- "$SWHKD_TRUSTED_DIR")"
    trusted_grandparent="$(dirname -- "$trusted_parent")"
    if [[ -e "$trusted_parent" || -L "$trusted_parent" ]]; then
        swhkd_root_owned_directory_is_safe "$trusted_parent" \
            || fail "$trusted_parent is not a safe root-owned directory."
    else
        swhkd_root_owned_directory_is_safe "$trusted_grandparent" \
            || fail "$trusted_grandparent is not a safe root-owned directory."
        as_root install -d -o root -g root -m755 "$trusted_parent"
    fi

    as_root install -d -o root -g root -m755 "$SWHKD_TRUSTED_DIR"
    swhkd_directory_is_privileged_safe "$SWHKD_TRUSTED_DIR" \
        || fail "$SWHKD_TRUSTED_DIR is not a safe privileged directory."
    as_root install -o root -g root -m755 "$source_dir/install-swhkd-helper.sh" "$SWHKD_TRUSTED_HELPER"
    as_root install -o root -g root -m755 "$source_dir/build-swhkd-locked.sh" "$SWHKD_TRUSTED_BUILD_SCRIPT"
    as_root install -o root -g root -m644 "$source_dir/swhkd-Cargo.lock.pinned" "$SWHKD_TRUSTED_PINNED_LOCK"
    printf 'managed-by: linux-soundboard\n' | as_root tee "$SWHKD_TRUSTED_MARKER" >/dev/null
    as_root chmod 644 "$SWHKD_TRUSTED_MARKER"
    trusted_swhkd_bundle_is_safe \
        || fail "The provisioned privileged helper bundle failed validation."
)
install_appimage() {
    local tag=${1:-}
    local image
    local extract_dir="$WORK_DIR/appimage"
    local installer effective_tag prior_version="${LSB_INSTALL_VERSION:-}"

    ensure_appimage_host_compatibility
    ensure_appimage_host_libraries
    ensure_fuse_for_appimage
    download_appimage "$tag" >/dev/null
    image="$DOWNLOADED_APPIMAGE"

    mkdir -p "$extract_dir"
    info "Extracting..."
    ( cd "$extract_dir" && "$image" --appimage-extract >/dev/null ) \
        || fail "Could not unpack the AppImage. Run it directly and choose 'Install for persistent virtual mic'."

    installer="$extract_dir/squashfs-root/usr/libexec/$APP_BINARY/installer/install-user.sh"
    [[ -f "$installer" ]] || fail "This AppImage carries no bundled installer."
    [[ -x "$installer" ]] || chmod +x "$installer"

    effective_tag="$tag"
    [[ -n "$effective_tag" ]] || effective_tag="$(get_release_json | release_tag_in || true)"
    if [[ -n "$effective_tag" ]]; then
        LSB_INSTALL_VERSION="$(normalize_release_version "$effective_tag")"
        export LSB_INSTALL_VERSION
    fi

    if is_wayland; then
        info "Provisioning the authenticated Wayland hotkey helper..."
        provision_trusted_swhkd_helper_from_verified_appimage "$image" "$VERIFIED_ASSET_SHA256"
    fi

    "$installer" install "$image"

    if [[ -n "$prior_version" ]]; then
        export LSB_INSTALL_VERSION="$prior_version"
    else
        unset LSB_INSTALL_VERSION || true
    fi
}
install_auto() {
    warn_if_native_package_shadows || return 0
    install_appimage
}
warn_if_native_package_shadows() {
    installed_native_packages >/dev/null 2>&1 || return 0

    warn "A native $APP_PACKAGE package is installed; it would shadow this user install."
    if [[ -t 0 ]] && confirm "Remove the native package first?"; then
        remove_native_packages
        return 0
    fi

    info "Nothing was installed. Remove the legacy package first; current releases are AppImage-only."
    return 1
}
install_native() {
    case "$DISTRO_FAMILY" in
        arch)    install_arch   ;;
        debian)  install_debian ;;
        fedora)  install_fedora ;;
        *) fail "No native package is published for $DISTRO_NAME. Current releases use --method appimage." ;;
    esac
}
set_install_method() {
    case "$1" in
        auto|appimage|tarball|native) INSTALL_METHOD="$1" ;;
        binary)                       INSTALL_METHOD="tarball" ;;
        *) fail "Unknown install method: $1. Choose auto or appimage for current releases; tarball/native are legacy compatibility modes." ;;
    esac
}
trusted_system_command() {
    local safe_path="/usr/sbin:/usr/bin:/sbin:/bin"
    local command_name=$1
    local resolved

    if [[ "$command_name" == /* ]]; then
        resolved="$command_name"
    else
        resolved="$(PATH="$safe_path" command -v -- "$command_name" || true)"
    fi
    [[ -n "$resolved" && -x "$resolved" ]] || return 1
    printf '%s\n' "$resolved"
}
as_root() {
    local safe_path="/usr/sbin:/usr/bin:/sbin:/bin"
    local command_name=$1
    local resolved
    local sudo_path
    local env_path
    shift

    resolved="$(trusted_system_command "$command_name" || true)"
    [[ -n "$resolved" ]] || fail "Trusted system command not found: $command_name"

    if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
        PATH="$safe_path" "$resolved" "$@"
        return
    fi

    sudo_path="$(trusted_system_command sudo || true)"
    env_path="$(trusted_system_command env || true)"
    [[ -n "$sudo_path" ]] || fail "sudo is required for this step, and it is not installed."
    [[ -n "$env_path" ]] || fail "env is required for privileged command execution."
    "$sudo_path" "$env_path" "PATH=$safe_path" "$resolved" "$@"
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
        for pkg in "$APP_AUR_PACKAGE" "$APP_AUR_LEGACY_PACKAGE"; do
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
remove_managed_swhkd_assets() {
    if [[ -d "$SWHKD_MANAGED_DIR" && ! -L "$SWHKD_MANAGED_DIR" ]]; then
        if swhkd_directory_is_privileged_safe "$SWHKD_MANAGED_DIR" \
            && managed_marker_is_safe "$SWHKD_MANAGED_MARKER"; then
            as_root rm -f -- "$SWHKD_MANAGED_BIN" "$SWHKS_MANAGED_BIN" "$SWHKD_MANAGED_MARKER"
            as_root rmdir "$SWHKD_MANAGED_DIR" >/dev/null 2>&1 || true
            info "Removed managed Wayland hotkey daemon."
        else
            warn "Left $SWHKD_MANAGED_DIR in place because its ownership marker or directory safety check failed."
        fi
    fi

    if [[ -d "$SWHKD_TRUSTED_DIR" && ! -L "$SWHKD_TRUSTED_DIR" ]]; then
        if swhkd_directory_is_privileged_safe "$SWHKD_TRUSTED_DIR" \
            && managed_marker_is_safe "$SWHKD_TRUSTED_MARKER"; then
            as_root rm -f -- "$SWHKD_TRUSTED_HELPER" "$SWHKD_TRUSTED_BUILD_SCRIPT" \
                "$SWHKD_TRUSTED_PINNED_LOCK" "$SWHKD_TRUSTED_MARKER"
            as_root rmdir "$SWHKD_TRUSTED_DIR" >/dev/null 2>&1 || true
            info "Removed managed Wayland hotkey helper."
        else
            warn "Left $SWHKD_TRUSTED_DIR in place because it is package-owned or failed the safety check."
        fi
    fi
}
print_swhkd_security_status() {
    local helper_state managed_state="missing"
    if trusted_swhkd_bundle_is_safe; then helper_state="ready"
    elif [[ -e "$SWHKD_TRUSTED_HELPER" || -L "$SWHKD_TRUSTED_HELPER" ]]; then helper_state="unsafe/incomplete"
    else helper_state="missing"
    fi

    if swhkd_managed_binary_is_safe && [[ -u "$SWHKD_MANAGED_BIN" ]]; then managed_state="ready"
    elif [[ -e "$SWHKD_MANAGED_BIN" || -L "$SWHKD_MANAGED_BIN" ]]; then managed_state="unsafe/incomplete"
    fi

    printf '  Wayland helper: %s (%s)\n' "$helper_state" "$SWHKD_TRUSTED_HELPER"
    printf '  Managed swhkd:  %s (%s)\n' "$managed_state" "$SWHKD_MANAGED_BIN"
}
remove_installation() {
    parse_wrapper_remove_args "$@"
    run_user_installer_from_available_source remove "${USER_REMOVE_ARGS[@]}"

    if ((REMOVE_KEEP_PACKAGE == 1)); then
        info "Keeping native package because --keep-package was passed."
        if ! installed_native_packages >/dev/null 2>&1; then
            remove_managed_swhkd_assets
        fi
    else
        remove_native_packages
        remove_managed_swhkd_assets
    fi
}
print_status() {
    local installer app_path
    if installer="$(local_user_installer)"; then
        [[ -x "$installer" ]] || chmod +x "$installer"
        "$installer" status
    else
        app_path="$(resolved_app_binary || true)"
        printf '%s status:
' "$APP_NAME"
        printf '  Binary:         %s
' "${app_path:-not installed}"
        printf '  Version marker: %s
' "$(installed_version || printf 'missing')"
    fi
    print_native_package_status
    print_swhkd_security_status
}
swhkd_trusted_helper_is_safe() {
    local path="$1"
    [[ -n "$path" && -f "$path" && ! -L "$path" ]] || return 1
    swhkd_directory_is_privileged_safe "$(dirname -- "$path")" || return 1
    local uid mode
    uid="$(stat -c '%u' -- "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' -- "$path" 2>/dev/null || true)"
    [[ "$uid" == "0" && -n "$mode" ]] || return 1
    (( (8#$mode & 8#0022) == 0 )) || return 1
    (( (8#$mode & 8#0111) != 0 ))
}
root_owned_regular_file_is_safe() {
    local path=$1
    local executable=${2:-0}
    [[ -f "$path" && ! -L "$path" ]] || return 1
    local uid mode
    uid="$(stat -c '%u' -- "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' -- "$path" 2>/dev/null || true)"
    [[ "$uid" == "0" && -n "$mode" ]] || return 1
    (( (8#$mode & 8#0022) == 0 )) || return 1
    (( executable == 0 )) || (( (8#$mode & 8#0111) != 0 ))
}
managed_marker_is_safe() {
    local marker=$1
    root_owned_regular_file_is_safe "$marker" 0 \
        && [[ "$(cat "$marker" 2>/dev/null || true)" == "managed-by: linux-soundboard" ]]
}
trusted_swhkd_bundle_is_safe() {
    swhkd_trusted_helper_is_safe "$SWHKD_TRUSTED_HELPER" || return 1
    root_owned_regular_file_is_safe "$SWHKD_TRUSTED_BUILD_SCRIPT" 1 || return 1
    root_owned_regular_file_is_safe "$SWHKD_TRUSTED_PINNED_LOCK" 0 || return 1
    managed_marker_is_safe "$SWHKD_TRUSTED_MARKER" || return 1

    local expected_build expected_lock actual_build actual_lock
    expected_build="$(sed -n 's/^SWHKD_BUILD_SCRIPT_SHA256="\([0-9a-fA-F]\{64\}\)"$/\1/p' "$SWHKD_TRUSTED_HELPER" | head -n1)"
    expected_lock="$(sed -n 's/^SWHKD_PINNED_LOCK_SHA256="\([0-9a-fA-F]\{64\}\)"$/\1/p' "$SWHKD_TRUSTED_HELPER" | head -n1)"
    [[ -n "$expected_build" && -n "$expected_lock" ]] || return 1
    actual_build="$(sha256_of "$SWHKD_TRUSTED_BUILD_SCRIPT")" || return 1
    actual_lock="$(sha256_of "$SWHKD_TRUSTED_PINNED_LOCK")" || return 1
    [[ "${actual_build,,}" == "${expected_build,,}" && "${actual_lock,,}" == "${expected_lock,,}" ]]
}
ensure_trusted_swhkd_helper() {
    trusted_swhkd_bundle_is_safe && return 0

    local tag image
    tag="$(installed_version || true)"
    info "Restoring the trusted Wayland helper from a signed release AppImage..."
    download_appimage "$tag" >/dev/null
    image="$DOWNLOADED_APPIMAGE"
    provision_trusted_swhkd_helper_from_verified_appimage "$image" "$VERIFIED_ASSET_SHA256"
}
install_swhkd_via_trusted_helper() {
    local helper="$SWHKD_TRUSTED_HELPER"
    trusted_swhkd_bundle_is_safe || return 1
    as_root "$helper" --distro "$DISTRO_FAMILY"
}
print_manual_swhkd_recipe() {
    info "Wayland hotkey setup needs Linux Soundboard's fixed root-owned helper."
    info "AppImage installs do not elevate helpers from user-writable paths."
    info "Install install-swhkd-helper.sh, build-swhkd-locked.sh, and swhkd-Cargo.lock.pinned from a trusted source checkout under /usr/libexec/linux-soundboard, then run:"
    info "  sudo $SWHKD_TRUSTED_HELPER --distro $DISTRO_FAMILY"
}
swhkd_binary_is_safe() {
    local binary="$1"
    [[ -r "$binary" ]] || return 1
    if LC_ALL=C grep -aF -e '/dev/rfkill' -e 'SW_RFKILL_ALL' "$binary" >/dev/null 2>&1; then
        return 1
    else
        [[ $? -eq 1 ]]
    fi
}
swhkd_root_owned_directory_is_safe() {
    local dir="$1"
    [[ -n "$dir" && -d "$dir" && ! -L "$dir" ]] || return 1
    local uid mode
    uid="$(stat -c '%u' -- "$dir" 2>/dev/null || true)"
    mode="$(stat -c '%a' -- "$dir" 2>/dev/null || true)"
    [[ "$uid" == "0" && -n "$mode" ]] || return 1
    (( (8#$mode & 8#0022) == 0 ))
}
swhkd_directory_is_privileged_safe() {
    local dir="$1"
    swhkd_root_owned_directory_is_safe "$dir" || return 1
    swhkd_root_owned_directory_is_safe "$(dirname -- "$dir")"
}
swhkd_privileged_target_is_safe() {
    local path="$1"
    [[ -n "$path" && -f "$path" && ! -L "$path" ]] || return 1
    swhkd_directory_is_privileged_safe "$(dirname -- "$path")" || return 1
    local uid mode
    uid="$(stat -c '%u' -- "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' -- "$path" 2>/dev/null || true)"
    [[ "$uid" == "0" && -n "$mode" ]] || return 1
    (( (8#$mode & 8#0022) == 0 )) || return 1
    (( (8#$mode & 8#0111) != 0 ))
}
swhkd_managed_binary_is_safe() {
    swhkd_privileged_target_is_safe "$SWHKD_MANAGED_BIN"
}
configure_managed_swhkd_permissions() {
    [[ -e "$SWHKD_MANAGED_BIN" ]] || return 0
    if ! swhkd_managed_binary_is_safe; then
        warn "Managed swhkd at $SWHKD_MANAGED_BIN is not a regular root-owned executable; it will be rebuilt."
        return 1
    fi

    info "Configuring swhkd permissions..."
    as_root chown root:root "$SWHKD_MANAGED_BIN"
    as_root chmod u+s "$SWHKD_MANAGED_BIN"
    if [[ -f "$SWHKS_MANAGED_BIN" && ! -L "$SWHKS_MANAGED_BIN" ]]; then
        as_root chmod 755 "$SWHKS_MANAGED_BIN"
    fi

    [[ -u "$SWHKD_MANAGED_BIN" ]] || fail "swhkd setuid bit was not applied to $SWHKD_MANAGED_BIN."

    offer_uinput
}
resolve_swhkd_binary() {
    if swhkd_managed_binary_is_safe; then
        printf '%s\n' "$SWHKD_MANAGED_BIN"
        return 0
    fi
    local found
    found="$(command -v swhkd 2>/dev/null || true)"
    if [[ -n "$found" ]] && swhkd_privileged_target_is_safe "$found" && [[ -u "$found" ]]; then
        printf '%s\n' "$found"
        return 0
    fi
    return 1
}
resolve_swhks_binary() {
    if [[ -f "$SWHKS_MANAGED_BIN" && ! -L "$SWHKS_MANAGED_BIN" ]]; then
        printf '%s\n' "$SWHKS_MANAGED_BIN"
        return 0
    fi
    local found
    found="$(command -v swhks 2>/dev/null || true)"
    if [[ -n "$found" ]] && swhkd_privileged_target_is_safe "$found"; then
        printf '%s\n' "$found"
        return 0
    fi
    return 1
}
uinput_available() {
    [[ -d /sys/module/uinput ]] && return 0
    as_root sh -c 'exec 3>/dev/uinput' >/dev/null 2>&1
}
uinput_manual_commands() {
    printf '    sudo modprobe uinput\n'
    printf '    echo uinput | sudo tee /etc/modules-load.d/uinput.conf\n'
}
offer_uinput() {
    uinput_available && return 0

    local release; release="$(uname -r)"

    if [[ ! -d "/usr/lib/modules/$release" && ! -d "/lib/modules/$release" ]]; then
        warn "The running kernel ($release) has no modules on disk; it was replaced since boot."
        warn "Reboot, then run this again so uinput can load."
        return 0
    fi

    warn "swhkd needs the uinput kernel module, and this system does not provide it."
    printf '  swhkd reads your keyboards directly, so it has to type every key it does not\n'
    printf '  claim back to the system through a virtual keyboard. The uinput module is what\n'
    printf '  creates that keyboard; without it swhkd exits at startup and hotkeys stay dead.\n'
    printf '  Loading it changes nothing else, and listing it in /etc/modules-load.d keeps\n'
    printf '  hotkeys working after a restart.\n\n'

    if [[ ! -t 0 ]]; then
        warn "No terminal to ask on, so nothing was loaded. To do it yourself:"
        uinput_manual_commands
        return 0
    fi

    if ! confirm "  Load uinput now and at every boot?"; then
        info "Left as it is. To do it later:"
        uinput_manual_commands
        return 0
    fi

    enable_uinput
}
enable_uinput() {
    if [[ ! -d /sys/module/uinput ]] && ! as_root modprobe uinput 2>/dev/null; then
        warn "Could not load the uinput module; Wayland hotkeys stay unavailable until it is."
        return 0
    fi

    [[ -f /etc/modules-load.d/uinput.conf ]] && return 0
    info "Loading uinput at boot via /etc/modules-load.d/uinput.conf"
    printf 'uinput\n' | as_root tee /etc/modules-load.d/uinput.conf >/dev/null
}
swhkd_requires_pkexec() {
    local swhkd_path
    local swhks_path
    local output_file
    local swhks_pid=""
    local status=0

    swhkd_path="$(resolve_swhkd_binary || true)"
    swhks_path="$(resolve_swhks_binary || true)"

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
    local existing=""
    existing="$(resolve_swhkd_binary || true)"

    if [[ -n "$existing" ]] && swhkd_binary_is_safe "$existing"; then
        if [[ "$existing" != "$SWHKD_MANAGED_BIN" ]]; then
            info "Using the existing safe swhkd install; leaving its permissions unchanged."
            offer_uinput
            if ! swhkd_requires_pkexec; then
                return
            fi
        elif configure_managed_swhkd_permissions; then
            if ! swhkd_requires_pkexec; then
                return
            fi
        fi
    elif [[ -n "$existing" ]]; then
        warn "Installed swhkd contains rfkill support or could not be verified; rebuilding it safely before launch."
    fi

    info "Installing swhkd with the root-side helper for Wayland hotkeys..."
    info "swhkd captures every keyboard on this machine; it is meant for single-seat systems."

    if ! install_swhkd_via_trusted_helper; then
        warn "Refusing to build swhkd as your user; $SWHKD_TRUSTED_HELPER is missing or unsafe."
        print_manual_swhkd_recipe
        return
    fi

    configure_managed_swhkd_permissions \
        || fail "Managed swhkd at $SWHKD_MANAGED_BIN could not be configured after the build."
}
repair_swhkd_if_needed() {
    if is_wayland; then
        install_swhkd
    elif [[ -n "$(resolve_swhkd_binary || true)" ]]; then
        configure_managed_swhkd_permissions || true
    fi
}
ensure_pipewire_services() {
    command -v systemctl >/dev/null 2>&1 || return
    local svc
    for svc in pipewire.service wireplumber.service; do
        if systemctl --user list-unit-files "$svc" >/dev/null 2>&1; then
            systemctl --user enable --now "$svc" >/dev/null 2>&1 || true
        fi
    done
}
install_main() {
    detect_distro
    detect_session
    info "Distro:  $DISTRO_NAME"
    info "Session: $SESSION_TYPE"

    case "$INSTALL_METHOD" in
        appimage)
            warn_if_native_package_shadows || return 0
            install_appimage
            ;;
        tarball)
            warn_if_native_package_shadows || return 0
            install_tarball
            ;;
        native) install_native ;;
        *)      install_auto   ;;
    esac

    if is_wayland; then
        install_swhkd
    fi

    ensure_pipewire_services

    print_launch_hint
}
print_launch_hint() {
    local user_binary="$HOME/.local/opt/$APP_BINARY/$APP_BINARY"

    printf '\n'
    if command -v "$APP_BINARY" >/dev/null 2>&1; then
        printf 'Done. Launch with: %s\n' "$APP_BINARY"
    elif [[ -x "$user_binary" ]]; then
        printf 'Done. Launch it from your applications menu, or run:\n  %s\n' "$user_binary"
    else
        printf 'Done.\n'
    fi
}
repair_main() {
    detect_distro
    detect_session

    if [[ $# -eq 0 ]] && installed_native_packages >/dev/null 2>&1; then
        info "Native Linux Soundboard package detected; configuring the user service only."
        run_user_installer_from_available_source setup-user
    else
        run_user_installer_from_available_source repair "$@"
    fi

    if is_wayland; then
        ensure_trusted_swhkd_helper
    fi
    repair_swhkd_if_needed
    ensure_pipewire_services
}
ensure_tty() {
    [[ -t 0 ]] && return 0

    ( exec </dev/tty ) 2>/dev/null || return 1
    exec </dev/tty
    [[ -t 0 ]]
}
confirm() {
    local prompt=$1
    local answer

    printf '%s [y/N] ' "$prompt"
    read -r answer || answer=""
    case "${answer,,}" in
        y|yes) return 0 ;;
        *)     return 1 ;;
    esac
}
print_menu_header() {
    local version
    local packages=()
    local kind
    local pkg

    while IFS=$'\t' read -r kind pkg; do
        [[ -n "${kind:-}" && -n "${pkg:-}" ]] || continue
        packages+=("$kind:$pkg")
    done < <(installed_native_packages || true)

    NATIVE_PACKAGE_PRESENT=$( ((${#packages[@]} > 0)) && printf 1 || printf 0)
    version="$(installed_version || true)"

    printf '\n'
    printf '  Linux Soundboard installer\n'
    printf '  ──────────────────────────\n'
    printf '  Distro:    %s\n' "${DISTRO_NAME:-unknown}"
    printf '  Session:   %s\n' "${SESSION_TYPE:-unknown}"
    printf '  Installed: %s\n' "${version:-not installed}"
    printf '  Package:   %s\n' "$( ((${#packages[@]} == 0)) && printf 'none' || printf '%s' "${packages[*]}")"
    printf '\n'
}
password_note() {
    case "$1" in
        install-newest)
            if is_wayland; then
                printf ' — AppImage needs no root; hotkey setup may ask for your password'
            else
                printf ' — no password needed'
            fi
            ;;
        install-previous)
            if ((NATIVE_PACKAGE_PRESENT == 1)); then
                printf ' — asks for your password only to remove the system package'
            else
                printf ' — no password needed'
            fi
            ;;
        uninstall)
            if ((NATIVE_PACKAGE_PRESENT == 1)); then
                printf ' — asks for your password (system package)'
            else
                printf ' — no password needed'
            fi
            ;;
        fix)
            if is_wayland; then
                printf ' — may ask for your password (hotkey daemon)'
            else
                printf ' — no password needed'
            fi
            ;;
        *)
            printf ' — no password needed'
            ;;
    esac
}
interactive_menu() {
    detect_distro
    detect_session

    while true; do
        print_menu_header
        printf '  1) Install the newest version%s\n'  "$(password_note install-newest)"
        printf '  2) Install a previous version%s\n'  "$(password_note install-previous)"
        printf '  3) Uninstall%s\n'                   "$(password_note uninstall)"
        printf '  4) Fix setup problems%s\n'          "$(password_note fix)"
        printf '  5) Make a bug report%s\n'           "$(password_note report)"
        printf '  6) Show status%s\n'                 "$(password_note status)"
        printf '  0) Exit\n'
        printf '\n  Choose an option: '

        local choice
        read -r choice || return 0

        case "$choice" in
            1) INSTALL_METHOD="appimage"; install_main ;;
            2) INSTALL_METHOD="appimage"; choose_and_install_version ;;
            3) remove_installation ;;
            4) fix_setup ;;
            5) make_bug_report ;;
            6) print_status ;;
            0) return 0 ;;
            "") ;;
            *) warn "Unknown option: $choice" ;;
        esac
    done
}
prompt_install_method() {
    local native=${1:-with-native}
    local choice

    printf '\n  Installation method:\n'
    printf '   1) AppImage — current supported release format (default)\n'
    printf '   2) Legacy binary tarball — historical releases only\n'
    [[ "$native" == "with-native" ]] && printf '   3) Legacy native package — historical workflows only\n'
    printf '\n  Choose a method [1]: '

    read -r choice || choice=""
    case "$choice" in
        ""|1) INSTALL_METHOD="appimage" ;;
        2)    INSTALL_METHOD="tarball" ;;
        3)
            if [[ "$native" != "with-native" ]]; then
                warn "Legacy native-package mode is not available here."
                return 1
            fi
            INSTALL_METHOD="native"
            ;;
        *) warn "Unknown option: $choice"; return 1 ;;
    esac
}
choose_and_install_version() {
    local tags=()
    local current
    local tag
    local index

    info "Reading published releases..."
    mapfile -t tags < <(list_release_tags | head -n 10)
    ((${#tags[@]} > 0)) || fail "No releases found for $APP_REPO."

    current="$(installed_version || true)"

    printf '\n  Published versions:\n'
    for index in "${!tags[@]}"; do
        tag="${tags[$index]}"
        printf '  %2d) %s%s\n' "$((index + 1))" "$tag" \
            "$([[ "$tag" == "$current" ]] && printf ' (installed)' || printf '')"
    done
    printf '   0) Back\n'
    printf '\n  Choose a version: '

    local choice
    read -r choice || return 0
    [[ "$choice" == "0" || -z "$choice" ]] && return 0
    [[ "$choice" =~ ^[0-9]+$ ]] || { warn "Not a number: $choice"; return 0; }
    ((choice >= 1 && choice <= ${#tags[@]})) || { warn "Out of range: $choice"; return 0; }

    install_version "${tags[$((choice - 1))]}"
}
install_version() {
    local tag=$1
    local bundle_dir

    detect_distro
    detect_session

    if installed_native_packages >/dev/null 2>&1; then
        warn "A native $APP_PACKAGE package is installed; it would shadow a user install of $tag."
        if confirm "Remove the native package first?"; then
            remove_native_packages
        else
            info "Leaving the native package in place. Nothing was installed."
            return 0
        fi
    fi

    LSB_INSTALL_VERSION="$(normalize_release_version "$tag")"
    export LSB_INSTALL_VERSION
    case "$INSTALL_METHOD" in
        native)
            unset LSB_INSTALL_VERSION
            fail "Native packages are a historical release format. Use --method appimage to install $tag."
            ;;
        auto|appimage)
            install_appimage "$tag"
            ;;
        tarball)
            ensure_runtime_dependencies
            bundle_dir="$(download_and_extract_tarball "$tag")"
            run_user_installer install "$bundle_dir"
            ;;
    esac
    unset LSB_INSTALL_VERSION

    if is_wayland; then
        repair_swhkd_if_needed
    fi
    ensure_pipewire_services

    printf '\n'
    info "Installed $tag."
    print_launch_hint
}
step() {
    local label=$1
    shift

    printf '  %-34s' "$label"

    if ( "$@" ) >"$WORK_DIR/step.log" 2>&1; then
        printf 'ok\n'
        return 0
    fi
    printf 'FAILED\n'
    sed 's/^/      /' "$WORK_DIR/step.log" | tail -n 5
    return 1
}
report_engine_mismatch() {
    local diagnosis=$1

    grep -q 'INCOMPATIBLE' "$diagnosis" || return 0

    printf '\n'
    warn "The running engine does not match the installed application."
    printf '    The engine service starts a different build than the app on your PATH,\n'
    printf '    so the app will refuse to talk to it. Install once so both come from\n'
    printf '    the same version:\n\n'
    printf '      ./install.sh install\n'
    return 1
}
fix_setup() {
    local failures=0

    detect_distro
    detect_session

    printf '\n  Repairing installation\n\n'
    step "user install and engine service" repair_main || failures=$((failures + 1))
    if is_wayland; then
        step "swhkd (Wayland hotkeys)" repair_swhkd_if_needed || failures=$((failures + 1))
    fi
    step "PipeWire services" ensure_pipewire_services || failures=$((failures + 1))

    printf '\n'
    print_status || true

    if command -v "$APP_BINARY" >/dev/null 2>&1; then
        local diagnosis="$WORK_DIR/diagnose.log"
        printf '\n'
        "$APP_BINARY" --diagnose >"$diagnosis" 2>&1 || true
        cat "$diagnosis"
        report_engine_mismatch "$diagnosis" || failures=$((failures + 1))
    fi

    if ((failures > 0)); then
        printf '\n'
        warn "$failures step(s) failed."
        if confirm "Make a bug report with this state?"; then
            make_bug_report
        fi
    fi
}
redact() {
    sed -e "s#$HOME#~#g" -e "s#\\b$(id -un)\\b#<user>#g"
}
section() {
    printf '\n================================================================\n'
    printf '%s\n' "$1"
    printf '================================================================\n\n'
}
run_or_note() {
    local label=$1
    shift

    printf -- '--- %s\n' "$label"
    if command -v "$1" >/dev/null 2>&1; then
        "$@" 2>&1 || printf '(command failed: %s)\n' "$*"
    else
        printf '(not installed: %s)\n' "$1"
    fi
    printf '\n'
}
collect_system_report() {
    section "SYSTEM REPORT"
    run_or_note "os-release" cat /etc/os-release
    run_or_note "kernel" uname -a
    printf -- '--- session\n'
    printf 'XDG_SESSION_TYPE=%s\nWAYLAND_DISPLAY=%s\nDISPLAY=%s\n\n' \
        "${XDG_SESSION_TYPE:-}" "${WAYLAND_DISPLAY:-}" "${DISPLAY:-}"
    run_or_note "audio devices" wpctl status -n
    run_or_note "audio services" systemctl --user --no-pager --lines=0 status pipewire wireplumber
    printf -- '--- swhkd\n'
    command -v swhkd >/dev/null 2>&1 && swhkd --version 2>&1 || printf 'not installed\n'
    printf '\n'
}
resolved_app_binary() {
    if command -v "$APP_BINARY" >/dev/null 2>&1; then
        command -v "$APP_BINARY"
    elif [[ -x "$INSTALL_ROOT/$APP_BINARY" ]]; then
        printf '%s\n' "$INSTALL_ROOT/$APP_BINARY"
    else
        return 1
    fi
}
collect_app_report() {
    local library="${XDG_CONFIG_HOME:-$HOME/.config}/$APP_BINARY/library.sqlite3"

    section "APP REPORT"
    printf -- '--- install\n'
    printf 'installed version: %s\n' "$(installed_version || printf 'not installed')"
    local app_path
    app_path="$(resolved_app_binary || true)"
    printf 'binary: %s\n' "${app_path:-not installed}"
    print_native_package_status
    print_swhkd_security_status
    printf '\n'
    if [[ -n "$app_path" ]]; then
        run_or_note "diagnose" "$app_path" --diagnose
    else
        printf '%s\n\n' '--- diagnose' '(application binary not installed)'
    fi
    run_or_note "engine service" systemctl --user --no-pager --lines=0 status "$APP_BINARY-engine.service"
    run_or_note "engine log" journalctl --user -u "$APP_BINARY-engine.service" -n 200 --no-pager
    printf -- '--- library\n'
    if [[ -f "$library" ]]; then
        printf 'file: %s (%s bytes)\n' "$library" "$(stat -c %s "$library")"
        if command -v sqlite3 >/dev/null 2>&1; then
            printf 'integrity: %s\n' "$(sqlite3 "$library" 'PRAGMA integrity_check;' 2>&1 | head -n 1)"
            printf 'schema: %s\n' "$(sqlite3 "$library" 'PRAGMA user_version;' 2>&1 | head -n 1)"
        else
            printf '(sqlite3 not installed; integrity not checked)\n'
        fi
    else
        printf 'no library database at %s\n' "$library"
    fi
    printf '\n'
    printf -- '--- audio changes since install\n'
    local installer
    if installer="$(local_user_installer)"; then
        bash "$installer" snapshot-diff 2>&1 || printf '(no snapshot recorded)\n'
    else
        printf '(install-user.sh not available here; run it from the app directory for the audio diff)\n'
    fi
    printf '\n'
}
collect_debug_run() {
    local raw_out=$1
    local log="$WORK_DIR/debug-run.log"

    local app_path
    app_path="$(resolved_app_binary || true)"
    [[ -n "$app_path" ]] || return 0
    printf '\n'
    printf 'A debug run starts Linux Soundboard and its audio engine, which changes\n'
    printf 'your default microphone while it runs, and records what the app logs.\n'
    confirm "Reproduce the problem now with debug logging?" || return 0

    info "Starting $APP_BINARY with RUST_LOG=debug ..."
    RUST_LOG=debug "$app_path" >"$log" 2>&1 &
    local pid=$!
    printf '\n  Reproduce the problem, then press Enter here.\n'
    read -r _ || true
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true

    { section "DEBUG RUN LOG (last 300 lines)"; tail -n 300 "$log"; } >>"$raw_out"
}
bug_report_blank() {
    cat <<'EOF'

================================================================
BUG REPORT — FILL THIS IN
================================================================

Write in your own words. Anything you can add helps.

What I was doing:


What I expected to happen:


What actually happened:


Does it happen every time? (always / sometimes / once):


Anything else worth knowing (recent updates, other audio apps running):


SCREENSHOTS — IMPORTANT
  Take screenshots of what you saw: the window, the error, the settings page.
  Screenshots cannot go in this text file. Attach them to the GitHub issue by
  dragging the image files into the issue description box.
EOF
}
make_bug_report() {
    local output=""
    local raw="$WORK_DIR/report.raw"

    while (($# > 0)); do
        case "$1" in
            --output) shift; output="${1:-}" ;;
            --output=*) output="${1#--output=}" ;;
            *) warn "Unknown report option: $1" ;;
        esac
        shift || true
    done
    [[ -n "$output" ]] || output="$HOME/linux-soundboard-bug-report-$(date -u +%Y%m%dT%H%M%SZ).txt"

    detect_distro
    detect_session

    info "Collecting system and application state..."
    {
        printf 'Linux Soundboard bug report\n'
        printf 'Generated: %s\n\n' "$(date -Is)"
        printf 'HOW TO USE THIS FILE\n'
        printf '  1. Read it before sharing. It lists your sound devices and services.\n'
        printf '     Your home path and username have already been replaced.\n'
        printf '  2. Fill in the BUG REPORT section at the bottom.\n'
        printf '  3. Open %s\n' "$ISSUE_URL"
        printf '  4. Paste this whole file into the issue, and attach your screenshots.\n'
        collect_system_report
        collect_app_report
    } >"$raw" 2>&1

    if [[ -t 0 ]]; then
        collect_debug_run "$raw" || true
    fi

    bug_report_blank >>"$raw"

    redact <"$raw" >"$output"
    chmod 600 "$output"

    printf '\n'
    info "Bug report written to: $output"
    printf '\n'
    printf '  Next steps:\n'
    printf '    1. Open the file and fill in the BUG REPORT section at the bottom.\n'
    printf '    2. Take screenshots of the problem.\n'
    printf '    3. Open %s and paste the file, then attach the screenshots.\n' "$ISSUE_URL"
    printf '\n'

    if [[ -t 0 ]] && command -v xdg-open >/dev/null 2>&1; then
        confirm "Open the new-issue page in your browser now?" \
            && (xdg-open "$ISSUE_URL" >/dev/null 2>&1 &)
    fi
}
main() {
    local command="${1:-}"

    if [[ -z "$command" ]]; then
        if ensure_tty; then
            [[ ${EUID:-$(id -u)} -eq 0 ]] && fail "Run as your regular user, not root."
            interactive_menu
            return
        fi
        command="install"
    fi

    case "$command" in
        --help|-h|help)
            usage
            return
            ;;
    esac

    [[ ${EUID:-$(id -u)} -eq 0 ]] && fail "Run as your regular user, not root."

    case "$command" in
        menu)
            ensure_tty || fail "No terminal available for the menu. Pass a command instead; see --help."
            interactive_menu
            ;;
        install)
            [[ $# -gt 0 ]] && shift
            local install_tag=""
            while [[ $# -gt 0 ]]; do
                case "$1" in
                    --version)
                        [[ -n "${2:-}" ]] || fail "--version needs a tag, for example v2.1.2."
                        install_tag="$2"; shift 2
                        ;;
                    --version=*)
                        install_tag="${1#--version=}"; shift
                        ;;
                    --method)
                        [[ -n "${2:-}" ]] || fail "--method needs a value: auto, appimage, tarball, or native."
                        set_install_method "$2"; shift 2
                        ;;
                    --method=*)
                        set_install_method "${1#--method=}"; shift
                        ;;
                    *)
                        fail "Unknown install option: $1. See --help."
                        ;;
                esac
            done

            ensure_tty || true
            if [[ -n "$install_tag" ]]; then
                install_version "$install_tag"
            else
                install_main
            fi
            ;;
        versions)
            list_release_tags
            ;;
        verify)
            [[ $# -gt 0 ]] && shift
            verify_local_download "$@"
            ;;
        repair|fix)
            [[ $# -gt 0 ]] && shift
            ensure_tty || true
            if [[ "$command" == "fix" ]]; then
                fix_setup
            else
                repair_main "$@"
            fi
            ;;
        report)
            [[ $# -gt 0 ]] && shift
            make_bug_report "$@"
            ;;
        status)
            [[ $# -gt 0 ]] && shift
            print_status
            ;;
        remove|uninstall)
            [[ $# -gt 0 ]] && shift
            ensure_tty || true
            remove_installation "$@"
            ;;
        *)
            usage
            exit 1
            ;;
    esac
}

main "$@"
