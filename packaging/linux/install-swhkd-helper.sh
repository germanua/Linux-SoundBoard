#!/usr/bin/env bash
set -euo pipefail
PATH="/usr/sbin:/usr/bin:/sbin:/bin"
export PATH

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SWHKD_REPO_URL="https://github.com/waycrate/swhkd.git"
SWHKD_UPSTREAM_COMMIT="cbbfc4a981aa263155e3216a42549c9a3ae645fe"
SWHKD_MANAGED_DIR="/usr/local/libexec/linux-soundboard"
SWHKD_MANAGED_BIN="$SWHKD_MANAGED_DIR/swhkd"
SWHKS_MANAGED_BIN="$SWHKD_MANAGED_DIR/swhks"
SWHKD_MANAGED_MARKER="$SWHKD_MANAGED_DIR/.managed-by-linux-soundboard"
SWHKD_BUILD_SCRIPT_SHA256="03c19d6db4a44ed15ea1c20758330218e64e68d75303b413c93ad7345bc64aed"
SWHKD_PINNED_LOCK_SHA256="b0ae3f27d0e371b76a5f033ab7137a7a5808d425ab588c38b8a0fa10231c81b1"


ENABLE_UINPUT=0

log() {
  printf '[swhkd-helper] %s\n' "$1"
}

fail() {
  log "ERROR: $1" >&2
  exit 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    return 1
  fi
}

any_sha256_tool_is_available() {
  command -v sha256sum >/dev/null 2>&1 \
    || command -v shasum >/dev/null 2>&1 \
    || command -v openssl >/dev/null 2>&1
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
      local missing=() pkg
      for pkg in git make rust pkgconf systemd gcc; do
        pacman -Qq "$pkg" >/dev/null 2>&1 || missing+=("$pkg")
      done
      if [ "${#missing[@]}" -gt 0 ]; then
        fail "Missing Arch build packages (${missing[*]}). Run 'sudo pacman -Syu' first, then retry."
      fi
      command -v cargo >/dev/null 2>&1 || fail "The installed Arch rust package does not provide cargo. Run 'sudo pacman -Syu' first, then retry."
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
      zypper --non-interactive install git make gcc cargo rust pkgconf-pkg-config systemd-devel
      ;;
    *)
      fail "Unsupported distribution family for one-click install."
      ;;
  esac
}



enable_uinput() {
  local release
  release="$(uname -r)"



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

pinned_build_input_dir() {
  local dir
  for dir in \
      "$SCRIPT_DIR" \
      "$SCRIPT_DIR/../libexec/linux-soundboard" \
      "$SCRIPT_DIR/../../libexec/linux-soundboard"; do
    if [ -f "$dir/build-swhkd-locked.sh" ] && [ -f "$dir/swhkd-Cargo.lock.pinned" ]; then
      printf '%s\n' "$dir"
      return 0
    fi
  done
  return 1
}

pinned_build_inputs_match() {
  local dir="$1"
  local actual

  [ -f "$dir/build-swhkd-locked.sh" ] || return 1
  [ -f "$dir/swhkd-Cargo.lock.pinned" ] || return 1
  actual="$(sha256_file "$dir/build-swhkd-locked.sh")" || return 1
  [ "$actual" = "$SWHKD_BUILD_SCRIPT_SHA256" ] || return 1
  actual="$(sha256_file "$dir/swhkd-Cargo.lock.pinned")" || return 1
  [ "$actual" = "$SWHKD_PINNED_LOCK_SHA256" ] || return 1
}

verify_pinned_build_inputs() {
  local dir="$1"

  any_sha256_tool_is_available \
    || fail "No sha256 tool is available; refusing to run unverified build inputs."
  pinned_build_inputs_match "$dir" \
    || fail "The pinned swhkd build inputs do not match their digests; refusing to run them."
}

root_owned_dir_is_safe() {
  local dir="$1"
  local uid mode

  [ -n "$dir" ] && [ -d "$dir" ] && [ ! -L "$dir" ] || return 1
  uid="$(stat -c '%u' -- "$dir" 2>/dev/null || true)"
  mode="$(stat -c '%a' -- "$dir" 2>/dev/null || true)"
  [ "$uid" = "0" ] && [ -n "$mode" ] || return 1
  (( (8#$mode & 8#0022) == 0 ))
}

managed_parent_is_safe() {
  local parent

  parent="$(dirname -- "$1")"
  [ -e "$parent" ] || [ -L "$parent" ] || return 0
  root_owned_dir_is_safe "$parent"
}

managed_dir_is_safe() {
  root_owned_dir_is_safe "$1" || return 1
  managed_parent_is_safe "$1"
}

managed_target_is_replaceable() {
  local path="$1"

  [ ! -e "$path" ] && [ ! -L "$path" ] && return 0
  [ -f "$path" ] && [ ! -L "$path" ]
}

install_managed_swhkd_binaries() {
  local checkout="$1"
  local source_bin="$checkout/target/release/swhkd"
  local source_srv="$checkout/target/release/swhks"

  if [ ! -f "$source_bin" ] || [ -L "$source_bin" ]; then
    fail "No regular swhkd binary in the build workspace."
  fi
  if [ ! -f "$source_srv" ] || [ -L "$source_srv" ]; then
    fail "No regular swhks binary in the build workspace."
  fi
  swhkd_binary_is_safe "$source_bin" \
    || fail "Built swhkd still contains rfkill support; refusing to install it."

  log "Installing binaries"
  if [ -e "$SWHKD_MANAGED_DIR" ] || [ -L "$SWHKD_MANAGED_DIR" ]; then
    managed_dir_is_safe "$SWHKD_MANAGED_DIR" \
      || fail "$SWHKD_MANAGED_DIR is not a root-owned, non-writable directory."
  else
    managed_parent_is_safe "$SWHKD_MANAGED_DIR" \
      || fail "$(dirname -- "$SWHKD_MANAGED_DIR") is not a root-owned, non-writable directory."
    install -d -m755 "$SWHKD_MANAGED_DIR"
  fi
  if ! managed_target_is_replaceable "$SWHKD_MANAGED_BIN" \
    || ! managed_target_is_replaceable "$SWHKS_MANAGED_BIN"; then
    fail "$SWHKD_MANAGED_DIR holds a symlink or special file."
  fi

  install -Dm755 "$source_bin" "$SWHKD_MANAGED_BIN"
  install -Dm755 "$source_srv" "$SWHKS_MANAGED_BIN"
  rm -f -- "$SWHKD_MANAGED_MARKER"
  printf 'managed-by: linux-soundboard\n' > "$SWHKD_MANAGED_MARKER"
  chmod 644 "$SWHKD_MANAGED_MARKER"

  local man
  for man in "$checkout"/docs/*.gz; do
    [ -e "$man" ] || continue
    case "$(basename "$man")" in
      *.1.gz) install -Dm644 "$man" "/usr/share/man/man1/$(basename "$man")" ;;
      *.5.gz) install -Dm644 "$man" "/usr/share/man/man5/$(basename "$man")" ;;
    esac
  done

  if [ ! -f /etc/swhkd/swhkdrc ]; then
    install -Dm644 /dev/null /etc/swhkd/swhkdrc
  fi

  chown root:root "$SWHKD_MANAGED_BIN"
  chmod u+s "$SWHKD_MANAGED_BIN"
  chmod 755 "$SWHKS_MANAGED_BIN"

  if [ ! -u "$SWHKD_MANAGED_BIN" ]; then
    fail "swhkd setuid bit was not applied."
  fi
}

build_and_install_swhkd() {
  local work_dir
  local build_input_dir

  work_dir="$(mktemp -d /tmp/linux-soundboard-swhkd.XXXXXX)"
  trap 'rm -rf "$work_dir"' EXIT

  log "Fetching pinned swhkd sources"
  git init "$work_dir/swhkd"
  git -C "$work_dir/swhkd" fetch --depth 1 "$SWHKD_REPO_URL" "$SWHKD_UPSTREAM_COMMIT"
  git -C "$work_dir/swhkd" checkout --detach "$SWHKD_UPSTREAM_COMMIT"

  build_input_dir="$(pinned_build_input_dir)" \
    || fail "The pinned swhkd build inputs are missing next to $0."
  managed_dir_is_safe "$build_input_dir" \
    || fail "$build_input_dir is not a root-owned, non-writable directory."
  verify_pinned_build_inputs "$build_input_dir"

  log "Building swhkd (pinned lockfile, no_rfkill)"
  "$build_input_dir/build-swhkd-locked.sh" "$work_dir/swhkd"

  install_managed_swhkd_binaries "$work_dir/swhkd"

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

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
