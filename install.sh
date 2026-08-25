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
APP_AUR_PACKAGE="linux-soundboard"
APP_AUR_LEGACY_PACKAGE="linux-soundboard-git"
SWHKD_REPO_URL="https://github.com/waycrate/swhkd.git"
SWHKD_UPSTREAM_COMMIT="cbbfc4a981aa263155e3216a42549c9a3ae645fe"
ISSUE_URL="https://github.com/$APP_REPO/issues/new"

XDG_STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
INSTALL_ROOT="${INSTALL_ROOT:-$HOME/.local/opt/$APP_BINARY}"
INSTALL_VERSION_FILE="$INSTALL_ROOT/.installed-version"

WORK_DIR="$(mktemp -d)"
LATEST_RELEASE_JSON=""
# Install method for this run: auto | appimage | tarball | native.
INSTALL_METHOD="auto"
RELEASE_LIST_JSON=""
NATIVE_PACKAGE_PRESENT=0
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
  ./install.sh                 open the menu (needs a terminal)
  ./install.sh menu
  ./install.sh install [--method auto|appimage|tarball|native]
  ./install.sh install --version vX.Y.Z [--method auto|appimage|tarball]
  ./install.sh versions
  ./install.sh repair [binary]
  ./install.sh fix
  ./install.sh report [--output PATH]
  ./install.sh status
  ./install.sh remove [--yes] [--keep-data] [--keep-package] [--restore-default-source|--keep-current-default-source]
  ./install.sh uninstall [--yes] [--keep-data] [--keep-package] [--restore-default-source|--keep-current-default-source]
  ./install.sh --help

With no arguments and a terminal available, the menu opens; piped with no
arguments it installs the newest version, which keeps scripted use working.

install            detects your distro and installs via a native package when available
--method           auto (default) keeps that detection; appimage and tarball install into
                   ~/.local without root; native forces the distro package
install --version  installs that published release into ~/.local, from its tarball or,
                   with --method appimage, from its AppImage
fix                repairs the install step by step and prints what failed
report             writes a bug report file with system state, app state, and a blank to fill in
remove/uninstall   removes per-user files and the native package unless --keep-package,
                   showing what changed in your audio setup before offering to restore it
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

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "$1 is required${2:+ $2}, and it is not installed."
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

# Releases publish SHA256SUMS.txt covering every asset. A missing list is only a
# warning so older releases stay installable; a mismatch always stops the install.
verify_download() {
    local file=$1
    local tag=${2:-}
    local sums="$WORK_DIR/SHA256SUMS.txt"
    local url expected actual

    if [[ -n "$tag" ]]; then
        url="$(release_json_for_tag "$tag" | find_asset_url_in "SHA256SUMS\\.txt$")"
    else
        url="$(find_asset_url "SHA256SUMS\\.txt$" || true)"
    fi

    if [[ -z "$url" ]] || ! fetch "$url" "$sums" 2>/dev/null; then
        warn "This release publishes no checksum list; skipping verification."
        return 0
    fi

    expected="$(awk -v name="$(basename "$file")" '$2 == name || $2 == "*" name { print $1; exit }' "$sums")"
    if [[ -z "$expected" ]]; then
        warn "$(basename "$file") is not listed in SHA256SUMS.txt; skipping verification."
        return 0
    fi

    if ! actual="$(sha256_of "$file")"; then
        warn "No sha256 tool found; skipping verification."
        return 0
    fi

    [[ "$actual" == "$expected" ]] \
        || fail "Checksum mismatch for $(basename "$file"). Expected $expected, got $actual. Download aborted."

    info "Checksum verified."
}

get_release_json() {
    if [[ -z "$LATEST_RELEASE_JSON" ]]; then
        LATEST_RELEASE_JSON="$(fetch_stdout "https://api.github.com/repos/$APP_REPO/releases/latest")" \
            || fail "Could not reach GitHub API."
    fi
    printf '%s' "$LATEST_RELEASE_JSON"
}

# The API is unauthenticated here, so it allows 60 requests an hour. Each list
# and each tag lookup is cached for the run.
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

# Reads asset URLs out of release JSON on stdin so the same matcher serves the
# latest release and a pinned tag.
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

is_wayland() { [[ "${SESSION_TYPE:-}" == "wayland" ]] || [[ -n "${WAYLAND_DISPLAY:-}" ]]; }

# ── Package manager helpers ───────────────────────────────────────────────────

apt_install() {
    if (( APT_UPDATED == 0 )); then as_root apt-get update; APT_UPDATED=1; fi
    as_root apt-get install -y "$@"
}

pacman_install()  { as_root pacman -S --needed --noconfirm "$@"; }
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

# ── App installation ──────────────────────────────────────────────────────────

# Download the release tarball into WORK_DIR and return the extracted bundle path.
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

    if command -v yay  >/dev/null 2>&1; then yay  -S --needed --noconfirm --useask "$APP_AUR_PACKAGE"; return; fi
    if command -v paru >/dev/null 2>&1; then paru -S --needed --noconfirm --useask "$APP_AUR_PACKAGE"; return; fi

    # No AUR helper — build manually
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

    # The package owns the binary, desktop entry, icons, and the systemd user
    # unit. Only enable the engine service for the installing account; do not
    # redeploy those files into ~/.local, which would shadow the package and run
    # a stale binary after a package upgrade. The package's postinst already
    # enables the service for new logins, so a failure here is non-fatal.
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

    # The package owns the binary, desktop entry, icons, and the systemd user
    # unit. Only enable the engine service for the installing account; do not
    # redeploy those files into ~/.local, which would shadow the package and run
    # a stale binary after a package upgrade. The package's postinst already
    # enables the service for new logins, so a failure here is non-fatal.
    run_user_installer_from_available_source setup-user \
        || warn "Could not configure the user service; it will start on next login."
}

# The tarball ships only the binary, so GTK, libadwaita, PulseAudio, Opus, X11,
# and the audio stack have to come from the distro — the same set the .deb, .rpm,
# and AUR packages declare. Checked by soname first: a desktop system normally
# has all of it, and this path should not ask for a password when it does not
# have to.
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
        arch)     pacman_install "${pkgs[@]}" ;;
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

# Download the release AppImage into WORK_DIR and return its path.
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

    printf '%s\n' "$image"
}

# The installed AppImage mounts itself on every launch, so FUSE has to be present
# on the machine afterwards — unpacking it here does not need it.
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
            arch)     pacman_install "${pkgs[@]}" ;;
            debian)   apt_install    "${pkgs[@]}" ;;
            fedora)   dnf_install    "${pkgs[@]}" ;;
            opensuse) zypper_install "${pkgs[@]}" ;;
        esac
    else
        warn "Continuing without FUSE. Install ${pkgs[*]} before launching the app."
    fi
}

# A type-2 AppImage mounts itself with FUSE 2: it needs both the library and the
# fusermount helper, which several distros ship in separate packages. Ubuntu
# 24.04 also renamed libfuse2 for the 64-bit time_t transition, so ask the
# package manager which names it actually carries.
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

# The AppImage carries the same install-user.sh that its own "Install for
# persistent virtual mic" button runs, so unpack it and hand it the image.
install_appimage() {
    local tag=${1:-}
    local image
    local extract_dir="$WORK_DIR/appimage"
    local installer

    ensure_fuse_for_appimage
    image="$(download_appimage "$tag")"

    mkdir -p "$extract_dir"
    info "Extracting..."
    ( cd "$extract_dir" && "$image" --appimage-extract >/dev/null ) \
        || fail "Could not unpack the AppImage. Run it directly and choose 'Install for persistent virtual mic'."

    installer="$extract_dir/squashfs-root/usr/libexec/$APP_BINARY/installer/install-user.sh"
    [[ -f "$installer" ]] || fail "This AppImage carries no bundled installer."
    [[ -x "$installer" ]] || chmod +x "$installer"

    "$installer" install "$image"
}

# Automatic: the distro's native package when the release ships one, and the
# ~/.local tarball everywhere else. This is what the installer has always done.
install_auto() {
    case "$DISTRO_FAMILY" in
        arch)    install_arch    ;;
        debian)  install_debian  ;;
        fedora)  install_fedora  ;;
        *)       install_tarball ;;
    esac
}

# A ~/.local install sits behind the packaged /usr/bin binary on PATH, so the two
# would disagree about which build the engine service runs.
warn_if_native_package_shadows() {
    installed_native_packages >/dev/null 2>&1 || return 0

    warn "A native $APP_PACKAGE package is installed; it would shadow this user install."
    if [[ -t 0 ]] && confirm "Remove the native package first?"; then
        remove_native_packages
        return 0
    fi

    info "Nothing was installed. Remove that package first, or use --method native to update it."
    return 1
}

install_native() {
    case "$DISTRO_FAMILY" in
        arch)    install_arch   ;;
        debian)  install_debian ;;
        fedora)  install_fedora ;;
        *) fail "No native package is published for $DISTRO_NAME. Use --method tarball or --method appimage." ;;
    esac
}

set_install_method() {
    case "$1" in
        auto|appimage|tarball|native) INSTALL_METHOD="$1" ;;
        binary)                       INSTALL_METHOD="tarball" ;;
        *) fail "Unknown install method: $1. Choose auto, appimage, tarball, or native." ;;
    esac
}

# ── Repair, status, and removal ───────────────────────────────────────────────

as_root() {
    if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        fail "sudo is required for this step, and it is not installed."
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
    git init "$src"
    git -C "$src" fetch --depth 1 "$SWHKD_REPO_URL" "$SWHKD_UPSTREAM_COMMIT"
    git -C "$src" checkout --detach "$SWHKD_UPSTREAM_COMMIT"
    (
        cd "$src"
        make clean 2>/dev/null || true
        make NO_RFKILL_SW_SUPPORT=1
    )
    swhkd_binary_is_safe "$src/target/release/swhkd" \
        || fail "Built swhkd still contains rfkill support; refusing to install it."
    as_root install -Dm755 "$src/target/release/swhkd" /usr/bin/swhkd
    as_root install -Dm755 "$src/target/release/swhks" /usr/bin/swhks
    for f in "$src"/docs/*.gz; do
        [[ -e "$f" ]] || continue
        case "$(basename "$f")" in
            *.1.gz) as_root install -Dm644 "$f" "/usr/share/man/man1/$(basename "$f")" ;;
            *.5.gz) as_root install -Dm644 "$f" "/usr/share/man/man5/$(basename "$f")" ;;
        esac
    done
    [[ -f /etc/swhkd/swhkdrc ]] || as_root install -Dm644 /dev/null /etc/swhkd/swhkdrc
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

configure_swhkd_permissions() {
    local swhkd_path
    local swhks_path

    swhkd_path="$(command -v swhkd 2>/dev/null || true)"
    swhks_path="$(command -v swhks 2>/dev/null || true)"

    [[ -n "$swhkd_path" ]] || fail "swhkd was not found after installation."
    [[ -n "$swhks_path" ]] || fail "swhks was not found after installation."

    info "Configuring swhkd permissions..."
    as_root chown root:root "$swhkd_path"
    as_root chmod u+s "$swhkd_path"
    as_root chmod +x "$swhks_path"

    [[ -u "$swhkd_path" ]] || fail "swhkd setuid bit was not applied to $swhkd_path."

    offer_uinput
}

# The /dev/uinput node exists even when the driver is absent, so opening it is the
# only honest probe: the kernel autoloads the module on open where it is present,
# and fails with ENODEV where it is not. Called right after the chown above, so
# the sudo timestamp is already warm and this asks for no extra password.
uinput_available() {
    [[ -d /sys/module/uinput ]] && return 0
    as_root sh -c 'exec 3>/dev/uinput' >/dev/null 2>&1
}

uinput_manual_commands() {
    printf '    sudo modprobe uinput\n'
    printf '    echo uinput | sudo tee /etc/modules-load.d/uinput.conf\n'
}

# Systems that already have uinput are left untouched. The rest are asked first:
# loading a kernel module and making it load at boot is the machine owner's call.
offer_uinput() {
    uinput_available && return 0

    local release; release="$(uname -r)"

    # A kernel upgrade removes the running kernel's module tree, so nothing can be
    # loaded until the new one is booted. Asking to modprobe here would only fail.
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
        local swhkd_path
        swhkd_path="$(command -v swhkd)"
        if swhkd_binary_is_safe "$swhkd_path"; then
            info "swhkd already installed; checking permissions."
            configure_swhkd_permissions
            if ! swhkd_requires_pkexec; then
                return
            fi
        else
            warn "Installed swhkd contains rfkill support or could not be verified; rebuilding it safely before launch."
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
            local pkgcfg; pkgcfg="$(pick_pkg pkg-config pkgconf-pkg-config || true)"
            local udevdev; udevdev="$(pick_pkg systemd-devel libudev-devel || true)"
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
        local swhkd_path
        swhkd_path="$(command -v swhkd)"
        if swhkd_binary_is_safe "$swhkd_path"; then
            configure_swhkd_permissions
        else
            warn "Installed swhkd contains rfkill support or could not be verified; leaving it unchanged."
        fi
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

# Only the native package lands in /usr/bin. Tarball and AppImage installs go to
# ~/.local/opt, which is not on PATH, so naming the binary there would mislead.
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

# ── Interactive front end ─────────────────────────────────────────────────────

# The one-liner pipes this script into bash, which makes stdin the script
# itself. Every prompt here and in install-user.sh reads stdin, so point it at
# the terminal once, up front. Returns non-zero when there is no terminal at
# all, which is what keeps piped noninteractive use working.
ensure_tty() {
    [[ -t 0 ]] && return 0
    # /dev/tty exists even with no controlling terminal, where opening it fails
    # with ENXIO. Probe in a subshell so the failure is silent and stdin here is
    # left alone; testing the redirection inline would either print the shell's
    # own error or permanently redirect stderr to hide it.
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

# Says up front which entries will ask for a password, from what is actually
# true here: only a native package install or removal and the setuid swhkd
# binary need root. Everything under ~/.local and ~/.config does not.
password_note() {
    case "$1" in
        install-newest)
            # The method chosen on the next screen decides: only the system
            # package needs a password, and AppImage and tarball never do.
            case "$DISTRO_FAMILY" in
                arch|debian|fedora|opensuse)
                    printf ' — password only for the system package'
                    ;;
                *)
                    if is_wayland; then
                        printf ' — asks for your password (hotkey daemon only)'
                    else
                        printf ' — no password needed'
                    fi
                    ;;
            esac
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
            1) prompt_install_method && install_main ;;
            2) prompt_install_method no-native && choose_and_install_version ;;
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

# Pressing enter keeps the previous one-keystroke behaviour. Returns non-zero on
# an unusable answer so the menu redraws instead of installing something else.
prompt_install_method() {
    local native=${1:-with-native}
    local choice

    printf '\n  Installation method:\n'
    printf '   1) Automatic — native package when available, binary otherwise\n'
    printf '   2) AppImage — self-contained, installs into ~/.local, no root\n'
    printf '   3) Binary tarball — installs into ~/.local, no root\n'
    [[ "$native" == "with-native" ]] && printf '   4) Native package — .deb, .rpm, or AUR\n'
    printf '\n  Choose a method [1]: '

    read -r choice || choice=""
    case "$choice" in
        ""|1) INSTALL_METHOD="auto" ;;
        2)    INSTALL_METHOD="appimage" ;;
        3)    INSTALL_METHOD="tarball" ;;
        4)
            if [[ "$native" != "with-native" ]]; then
                warn "Native packages carry the newest version only."
                return 1
            fi
            INSTALL_METHOD="native"
            ;;
        *) warn "Unknown option: $choice"; return 1 ;;
    esac
}

# ── Previous versions ─────────────────────────────────────────────────────────

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

# An older version always installs from its release tarball into ~/.local. The
# AUR only ever carries the newest version, and apt/dnf downgrades need flags
# that differ per distro, so the tarball is the one path that behaves the same
# everywhere and needs no root.
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

    export LSB_INSTALL_VERSION="$tag"
    case "$INSTALL_METHOD" in
        native)
            unset LSB_INSTALL_VERSION
            fail "Native packages are published for the newest release only. Use --method tarball or --method appimage to install $tag."
            ;;
        appimage)
            install_appimage "$tag"
            ;;
        *)
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

# ── Fix setup problems ────────────────────────────────────────────────────────

step() {
    local label=$1
    shift

    printf '  %-34s' "$label"
    # In a subshell: these steps call fail() on error, which exits. Without the
    # subshell the first failing step would abort the repair instead of being
    # reported and counted.
    if ( "$@" ) >"$WORK_DIR/step.log" 2>&1; then
        printf 'ok\n'
        return 0
    fi
    printf 'FAILED\n'
    sed 's/^/      /' "$WORK_DIR/step.log" | tail -n 5
    return 1
}

# --diagnose exits 0 even when the engine and the application do not match, so
# the report has to be read. A user who runs the repair, sees every step report
# ok and still cannot use the app has been told nothing. Returns non-zero when a
# mismatch was found.
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

# ── Bug report ────────────────────────────────────────────────────────────────

# Keeps device names, which contributors need for routing bugs, but takes the
# home path and username out so the file can be pasted into a public issue.
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

collect_app_report() {
    local library="${XDG_CONFIG_HOME:-$HOME/.config}/$APP_BINARY/library.sqlite3"

    section "APP REPORT"
    printf -- '--- install\n'
    printf 'installed version: %s\n' "$(installed_version || printf 'not installed')"
    printf 'binary: %s\n' "$(command -v "$APP_BINARY" || printf 'not on PATH')"
    print_native_package_status
    printf '\n'
    run_or_note "diagnose" "$APP_BINARY" --diagnose
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

    command -v "$APP_BINARY" >/dev/null 2>&1 || return 0
    printf '\n'
    printf 'A debug run starts Linux Soundboard and its audio engine, which changes\n'
    printf 'your default microphone while it runs, and records what the app logs.\n'
    confirm "Reproduce the problem now with debug logging?" || return 0

    info "Starting $APP_BINARY with RUST_LOG=debug ..."
    RUST_LOG=debug "$APP_BINARY" >"$log" 2>&1 &
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
            # Piped through bash, stdin is the script itself, so the questions
            # this path may ask (runtime libraries, FUSE, uinput) need the
            # terminal. Where there is none they are skipped, as before.
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
