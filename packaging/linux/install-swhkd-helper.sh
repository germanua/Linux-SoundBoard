#!/usr/bin/env bash
set -euo pipefail

SWHKD_REPO_URL="https://github.com/waycrate/swhkd.git"
SWHKD_UPSTREAM_COMMIT="cbbfc4a981aa263155e3216a42549c9a3ae645fe"

# Only set by --enable-uinput: the caller asks the user first.
ENABLE_UINPUT=0

log() {
  printf '[swhkd-helper] %s\n' "$1"
}

fail() {
  log "ERROR: $1" >&2
  exit 1
}

swhkd_binary_is_safe() {
  local binary="$1"
  [ -r "$binary" ] || return 1
  if LC_ALL=C grep -aF -e '/dev/rfkill' -e 'SW_RFKILL_ALL' "$binary" >/dev/null 2>&1; then
    return 1
  else
    [ "$?" -eq 1 ]
  fi
}

require_root() {
  if [ "${EUID:-$(id -u)}" -ne 0 ]; then
    fail "This helper must run as root."
  fi
}

detect_distro_family() {
  if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    source /etc/os-release
  fi

  local ids="${ID:-} ${ID_LIKE:-}"
  ids="$(printf '%s' "$ids" | tr '[:upper:]' '[:lower:]')"

  case "$ids" in
    *arch*|*manjaro*|*endeavouros*)
      printf 'arch'
      ;;
    *debian*|*ubuntu*|*linuxmint*|*pop*|*elementary*|*zorin*)
      printf 'debian'
      ;;
    *fedora*|*rhel*|*centos*|*rocky*|*almalinux*)
      printf 'fedora'
      ;;
    *opensuse*|*sles*|*suse*)
      printf 'opensuse'
      ;;
    *)
      printf 'other'
      ;;
  esac
}

install_build_deps() {
  local distro="$1"

  case "$distro" in
    arch)
      command -v pacman >/dev/null 2>&1 || fail "pacman not found on Arch-family system."
      pacman -Sy --noconfirm --needed git make rust cargo pkgconf systemd base-devel
      ;;
    debian)
      command -v apt-get >/dev/null 2>&1 || fail "apt-get not found on Debian-family system."
      apt-get update
      apt-get install -y git make build-essential pkg-config libudev-dev cargo rustc
      ;;
    fedora)
      command -v dnf >/dev/null 2>&1 || fail "dnf not found on Fedora-family system."
      dnf install -y git make gcc cargo rust pkgconf-pkg-config systemd-devel
      ;;
    opensuse)
      command -v zypper >/dev/null 2>&1 || fail "zypper not found on openSUSE-family system."
      zypper --non-interactive install git make gcc cargo rust pkg-config systemd-devel
      ;;
    *)
      fail "Unsupported distribution family for one-click install."
      ;;
  esac
}

# swhkd creates a virtual keyboard through /dev/uinput. The device node exists
# even when the module is absent, and opening it then fails with ENODEV.
enable_uinput() {
  local release
  release="$(uname -r)"

  # A kernel upgrade removes the running kernel's module tree, so nothing loads
  # until the new one is booted.
  if [ ! -d "/usr/lib/modules/$release" ] && [ ! -d "/lib/modules/$release" ]; then
    log "WARNING: the running kernel ($release) has no modules on disk; reboot, then run this again"
    return 0
  fi

  if [ ! -d /sys/module/uinput ] && ! modprobe uinput 2>/dev/null; then
    log "WARNING: could not load the uinput module; hotkeys will not work until it is available"
    return 0
  fi

  if [ ! -f /etc/modules-load.d/uinput.conf ]; then
    log "Loading uinput at boot via /etc/modules-load.d/uinput.conf"
    printf 'uinput\n' > /etc/modules-load.d/uinput.conf
    chmod 644 /etc/modules-load.d/uinput.conf
  fi
}

build_and_install_swhkd() {
  local work_dir
  work_dir="$(mktemp -d /tmp/linux-soundboard-swhkd.XXXXXX)"
  trap 'rm -rf "$work_dir"' EXIT

  log "Fetching pinned swhkd sources"
  git init "$work_dir/swhkd"
  git -C "$work_dir/swhkd" fetch --depth 1 "$SWHKD_REPO_URL" "$SWHKD_UPSTREAM_COMMIT"
  git -C "$work_dir/swhkd" checkout --detach "$SWHKD_UPSTREAM_COMMIT"

  log "Building swhkd"
  (
    cd "$work_dir/swhkd"
    make clean || true
    make NO_RFKILL_SW_SUPPORT=1
  )
  swhkd_binary_is_safe "$work_dir/swhkd/target/release/swhkd" \
    || fail "Built swhkd still contains rfkill support; refusing to install it."

  log "Installing binaries"
  install -Dm755 "$work_dir/swhkd/target/release/swhkd" /usr/bin/swhkd
  install -Dm755 "$work_dir/swhkd/target/release/swhks" /usr/bin/swhks

  if [ ! -f /etc/swhkd/swhkdrc ]; then
    install -Dm644 /dev/null /etc/swhkd/swhkdrc
  fi

  chown root:root /usr/bin/swhkd
  chmod u+s /usr/bin/swhkd
  chmod +x /usr/bin/swhks

  if [ ! -u /usr/bin/swhkd ]; then
    fail "swhkd setuid bit was not applied."
  fi

  if [ "$ENABLE_UINPUT" -eq 1 ]; then
    enable_uinput
  fi

  log "Installation completed successfully"
}

main() {
  require_root

  local distro=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --distro)
        shift
        [ "$#" -gt 0 ] || fail "Missing value for --distro"
        distro="$1"
        ;;
      --enable-uinput)
        ENABLE_UINPUT=1
        ;;
      *)
        fail "Unknown argument: $1"
        ;;
    esac
    shift
  done

  if [ -z "$distro" ]; then
    distro="$(detect_distro_family)"
  fi

  log "Using distro strategy: $distro"
  install_build_deps "$distro"
  build_and_install_swhkd
}

main "$@"
