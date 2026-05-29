#!/usr/bin/env bash
set -euo pipefail

SWHKD_REPO_URL="https://github.com/waycrate/swhkd.git"

log() {
  printf '[swhkd-helper] %s\n' "$1"
}

fail() {
  log "ERROR: $1" >&2
  exit 1
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

build_and_install_swhkd() {
  local work_dir
  work_dir="$(mktemp -d /tmp/linux-soundboard-swhkd.XXXXXX)"
  trap 'rm -rf "$work_dir"' EXIT

  log "Cloning swhkd sources"
  git clone --depth 1 "$SWHKD_REPO_URL" "$work_dir/swhkd"

  log "Building swhkd"
  (
    cd "$work_dir/swhkd"
    make clean || true
    make
  )

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
