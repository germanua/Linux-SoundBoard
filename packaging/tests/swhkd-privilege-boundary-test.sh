#!/usr/bin/env bash
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SUT="$REPO_ROOT/install.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

RECORD="$WORK/privileged.log"
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); printf 'ok   - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'FAIL - %s\n' "$1"; }

expect_reject() {
    if swhkd_privileged_target_is_safe "$2"; then
        bad "$1"
    else
        ok "$1"
    fi
}

expect_accept() {
    if swhkd_privileged_target_is_safe "$2"; then
        ok "$1"
    else
        bad "$1"
    fi
}

sed '/^main "\$@"$/d' "$SUT" > "$WORK/install-functions.sh"
source "$WORK/install-functions.sh"
set +eu +o pipefail

as_root() { printf 'as_root %s\n' "$*" >> "$RECORD"; }
fail() { printf 'fail %s\n' "$*" >> "$RECORD"; return 1; }
info() { :; }
warn() { :; }
offer_uinput() { :; }

STUB_ROOT_DIR=""
STUB_MODE=""
STUB_DIR_MODE="755"
stat() {
    local fmt=""
    if [[ "${1:-}" == "-c" ]]; then
        fmt="$2"
        shift 2
    else
        command stat "$@"
        return
    fi
    [[ "${1:-}" == "--" ]] && shift
    local target="${1:-}"
    if [[ -n "$STUB_PARENT" && "$target" == "$STUB_PARENT" ]]; then
        case "$fmt" in
            '%u') printf '0\n'; return 0 ;;
            '%a') printf '755\n'; return 0 ;;
        esac
    fi
    if [[ -n "$STUB_ROOT_DIR" && ( "$target" == "$STUB_ROOT_DIR" || "$target" == "$STUB_ROOT_DIR"/* ) ]]; then
        local mode="$STUB_MODE"
        [[ "$target" == "$STUB_ROOT_DIR" ]] && mode="$STUB_DIR_MODE"
        case "$fmt" in
            '%u') printf '0\n'; return 0 ;;
            '%a') printf '%s\n' "$mode"; return 0 ;;
        esac
    fi
    command stat -c "$fmt" -- "$target"
}

SWHKD_MANAGED_BIN="$WORK/absent/swhkd"
SWHKS_MANAGED_BIN="$WORK/absent/swhks"

mkdir -p "$WORK/pathbin"
fake_swhkd="$WORK/pathbin/swhkd"
fake_swhks="$WORK/pathbin/swhks"
printf '#!/bin/sh\nexit 0\n' > "$fake_swhkd"
printf '#!/bin/sh\nexit 0\n' > "$fake_swhks"
chmod 755 "$fake_swhkd" "$fake_swhks"
PATH="$WORK/pathbin:$PATH"

: > "$RECORD"
if resolved="$(resolve_swhkd_binary)" && [[ "$resolved" == "$fake_swhkd" ]]; then
    bad "a fake user-owned swhkd first in PATH must not be selected"
else
    ok "a fake user-owned swhkd first in PATH is never selected"
fi
if [[ -s "$RECORD" ]]; then
    bad "selecting swhkd must not run any privileged command"
else
    ok "selecting swhkd runs no privileged command"
fi

expect_reject "a user-owned regular file is rejected" "$fake_swhkd"

ln -s "$fake_swhkd" "$WORK/pathbin/swhkd-link"
expect_reject "a symlink is rejected" "$WORK/pathbin/swhkd-link"

other="$WORK/pathbin/other-executable"
printf '#!/bin/sh\nexit 0\n' > "$other"
chmod 755 "$other"
ln -s "$other" "$WORK/pathbin/to-other"
expect_reject "a symlink to another executable is rejected" "$WORK/pathbin/to-other"

mkdir -p "$WORK/rootowned"
accept="$WORK/rootowned/swhkd"
printf '#!/bin/sh\nexit 0\n' > "$accept"
chmod 4755 "$accept"
writable="$WORK/rootowned/writable"
printf '#!/bin/sh\nexit 0\n' > "$writable"
chmod 4666 "$writable"

STUB_ROOT_DIR="$WORK/rootowned"
STUB_PARENT="$WORK"
STUB_MODE="4755"
expect_accept "a regular root-owned non-writable file is accepted" "$accept"

STUB_MODE="4666"
expect_reject "a group/world-writable file is rejected" "$writable"

STUB_MODE="4775"
expect_reject "a group-writable file is rejected" "$accept"

STUB_MODE="4644"
expect_reject "a root-owned non-executable file is rejected" "$accept"

STUB_MODE="4755"
ln -s "$accept" "$WORK/rootowned/symlink-to-safe"
expect_reject "a symlink to a root-owned file is still rejected" "$WORK/rootowned/symlink-to-safe"

STUB_DIR_MODE="777"
expect_reject "a root-owned file in a world-writable directory is rejected" "$accept"
STUB_DIR_MODE="755"
STUB_ROOT_DIR=""

mkdir -p "$WORK/parent-unsafe/managed"
STUB_ROOT_DIR="$WORK/parent-unsafe/managed"
if swhkd_directory_is_privileged_safe "$WORK/parent-unsafe/managed"; then
    bad "a root-owned directory with a user-writable parent must be rejected"
else
    ok "a root-owned directory with a user-writable parent is rejected"
fi
STUB_ROOT_DIR=""
STUB_PARENT=""

mkdir -p "$WORK/managed"
managed_swhkd="$WORK/managed/swhkd"
managed_swhks="$WORK/managed/swhks"
printf '#!/bin/sh\nexit 0\n' > "$managed_swhkd"
printf '#!/bin/sh\nexit 0\n' > "$managed_swhks"
chmod 4755 "$managed_swhkd"
chmod 755 "$managed_swhks"
SWHKD_MANAGED_BIN="$managed_swhkd"
SWHKS_MANAGED_BIN="$managed_swhks"
STUB_ROOT_DIR="$WORK/managed"
STUB_PARENT="$WORK"
STUB_MODE="4755"

: > "$RECORD"
configure_managed_swhkd_permissions
if grep -qx "as_root chown root:root $managed_swhkd" "$RECORD" \
    && grep -qx "as_root chmod u+s $managed_swhkd" "$RECORD"; then
    ok "the managed swhkd copy receives chown and chmod u+s"
else
    bad "the managed swhkd copy must receive chown and chmod u+s"
fi
if grep -vE "$managed_swhkd|$managed_swhks" "$RECORD" | grep -q .; then
    bad "no privileged command may target a non-managed path"
else
    ok "no privileged command targets a PATH- or HOME-resolved path"
fi

PATH="$WORK/pathbin:$PATH"
if [[ "$(resolve_swhkd_binary || true)" == "$managed_swhkd" ]]; then
    ok "the managed copy is preferred over an unsafe PATH entry"
else
    bad "the managed copy must be preferred over an unsafe PATH entry"
fi
STUB_ROOT_DIR=""
STUB_PARENT=""

user_managed="$WORK/managed/user-owned-swhkd"
printf '#!/bin/sh\nexit 0\n' > "$user_managed"
chmod 4755 "$user_managed"
SWHKD_MANAGED_BIN="$user_managed"
if resolve_swhkd_binary >/dev/null 2>&1; then
    bad "a user-owned managed path must not be trusted"
else
    ok "a user-owned managed path is not trusted"
fi

mkdir -p "$WORK/home/.local/opt/linux-soundboard"
home_swhkd="$WORK/home/.local/opt/linux-soundboard/swhkd"
printf '#!/bin/sh\nexit 0\n' > "$home_swhkd"
chmod 4755 "$home_swhkd"
expect_reject "a HOME-installed swhkd is never a privileged target" "$home_swhkd"

mkdir -p "$WORK/wedge"
wedge_swhkd="$WORK/wedge/swhkd"
printf '#!/bin/sh\nexit 0\n' > "$wedge_swhkd"
chmod 4755 "$wedge_swhkd"
SWHKD_MANAGED_BIN="$wedge_swhkd"
SWHKS_MANAGED_BIN="$WORK/wedge/swhks"
: > "$RECORD"
if configure_managed_swhkd_permissions; then
    bad "an unsafe managed copy must not be hardened"
else
    ok "an unsafe managed copy is reported instead of hardened"
fi
if [[ -s "$RECORD" ]]; then
    bad "an unsafe managed copy must receive no privileged command"
else
    ok "an unsafe managed copy receives no privileged command"
fi

HELPER="$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"
PINNED_LOCK="$REPO_ROOT/packaging/linux/swhkd-Cargo.lock.pinned"

if grep -qF 'build_swhkd_from_source' "$SUT"; then
    bad "install.sh must not build swhkd in user space"
else
    ok "install.sh never builds swhkd in user space"
fi

if grep -qE 'SWHKD_REPO_URL|waycrate/swhkd|cargo build.*no_rfkill|make NO_RFKILL|target/release/swhkd' "$SUT"; then
    bad "install.sh must not fetch or run the swhkd build inputs"
else
    ok "install.sh never fetches or runs the swhkd build inputs"
fi

if grep -qE 'as_root[^;]*(\$WORK_DIR|\$src|target/release)' "$SUT"; then
    bad "no privileged command may reference a user workspace artifact"
else
    ok "no privileged command references a user workspace artifact"
fi

if grep -qF 'SWHKD_TRUSTED_HELPER=' "$SUT" && grep -qF 'install-swhkd-helper.sh' "$SUT"; then
    ok "install.sh delegates swhkd to the fixed root-owned helper"
else
    bad "install.sh must delegate swhkd to the fixed root-owned helper"
fi

if grep -qF 'SWHKD_BUILD_SCRIPT_SHA256=' "$HELPER" && grep -qF 'SWHKD_PINNED_LOCK_SHA256=' "$HELPER"; then
    ok "the helper pins the build-script and lockfile digests"
else
    bad "the helper must pin the build-script and lockfile digests"
fi

verify_line="$(grep -n 'verify_pinned_build_inputs' "$HELPER" 2>/dev/null | head -1 | cut -d: -f1)"
exec_line="$(grep -n 'build-swhkd-locked.sh" "\$work_dir/swhkd"' "$HELPER" 2>/dev/null | head -1 | cut -d: -f1)"
if [[ -n "$verify_line" && -n "$exec_line" && "$verify_line" -lt "$exec_line" ]]; then
    ok "the helper verifies the pinned inputs before executing them"
else
    bad "the helper must verify the pinned inputs before executing them"
fi

altered="$WORK/payload-altered"
marker="$WORK/altered-build-ran"
mkdir -p "$altered"
cp "$PINNED_LOCK" "$altered/swhkd-Cargo.lock.pinned"
cp "$HELPER" "$altered/install-swhkd-helper.sh"
printf '#!/usr/bin/env bash\ntouch %q\n' "$marker" > "$altered/build-swhkd-locked.sh"
chmod 755 "$altered/build-swhkd-locked.sh"

if ( set -euo pipefail
          source "$REPO_ROOT/packaging/linux/install-swhkd-helper.sh" 2>/dev/null
     pinned_build_inputs_match "$REPO_ROOT/packaging/linux" ) >/dev/null 2>&1; then
    ok "the unmodified build script and lockfile pass verification"
else
    bad "the unmodified build script and lockfile must pass verification"
fi

if ( set -euo pipefail
          source "$altered/install-swhkd-helper.sh" 2>/dev/null
     ! pinned_build_inputs_match "$altered" ) >/dev/null 2>&1; then
    ok "an altered build script fails verification while the lockfile is unchanged"
else
    bad "an altered build script must fail verification"
fi

if ( set -euo pipefail
     STUB_ROOT_DIR="$altered"
     STUB_PARENT="$WORK"
     STUB_DIR_MODE="755"
          source "$altered/install-swhkd-helper.sh"
     log() { :; }
     git() { :; }
     build_and_install_swhkd ) >/dev/null 2>&1; then
    bad "the helper must reject an altered build script"
else
    ok "the helper rejects an altered build script"
fi

if [[ -e "$marker" ]]; then
    bad "an altered build script must never execute"
else
    ok "an altered build script never executed"
fi

verify_canary="$WORK/digest-routine-ran"
rm -f "$verify_canary"
if ( set -euo pipefail
     STUB_ROOT_DIR=""
          source "$altered/install-swhkd-helper.sh"
     log() { :; }
     git() { :; }
     verify_pinned_build_inputs() { touch "$verify_canary"; return 1; }
     build_and_install_swhkd ) >/dev/null 2>&1; then
    bad "a user-owned build-input directory must be refused"
else
    ok "a user-owned build-input directory is refused"
fi
if [[ -e "$verify_canary" ]]; then
    bad "a user-owned build-input directory must be refused before the digest routine runs"
else
    ok "a user-owned build-input directory is refused before the digest routine runs"
fi
if [[ -e "$marker" ]]; then
    bad "a refused build-input directory must execute nothing"
else
    ok "a refused build-input directory executes nothing"
fi

SWHKD_TRUSTED_HELPER="$WORK/no-such-helper"
: > "$RECORD"
if install_swhkd_via_trusted_helper; then
    bad "a missing trusted helper must not be used"
else
    ok "a missing trusted helper is refused"
fi
if [[ -s "$RECORD" ]]; then
    bad "a missing trusted helper must receive no privileged command"
else
    ok "a missing trusted helper receives no privileged command"
fi

untrusted="$WORK/untrusted-helper"
mkdir -p "$untrusted"
printf '#!/bin/sh\nexit 0\n' > "$untrusted/install-swhkd-helper.sh"
chmod 755 "$untrusted/install-swhkd-helper.sh"
SWHKD_TRUSTED_HELPER="$untrusted/install-swhkd-helper.sh"
: > "$RECORD"
if install_swhkd_via_trusted_helper; then
    bad "a user-owned helper must not be executed with privilege"
else
    ok "a user-owned helper is never executed with privilege"
fi
if [[ -s "$RECORD" ]]; then
    bad "a user-owned helper must receive no privileged command"
else
    ok "a user-owned helper receives no privileged command"
fi

trusted_dir="$WORK/trusted-helper"
trusted="$trusted_dir/install-swhkd-helper.sh"
trusted_build="$trusted_dir/build-swhkd-locked.sh"
trusted_lock="$trusted_dir/swhkd-Cargo.lock.pinned"
trusted_marker="$trusted_dir/.managed-by-linux-soundboard"
mkdir -p "$trusted_dir"
printf '#!/bin/sh\nexit 0\n' > "$trusted_build"
printf 'pinned-lock\n' > "$trusted_lock"
build_hash="$(sha256sum "$trusted_build" | awk '{print $1}')"
lock_hash="$(sha256sum "$trusted_lock" | awk '{print $1}')"
printf '#!/bin/sh\nSWHKD_BUILD_SCRIPT_SHA256="%s"\nSWHKD_PINNED_LOCK_SHA256="%s"\nexit 0\n' \
    "$build_hash" "$lock_hash" > "$trusted"
printf 'managed-by: linux-soundboard\n' > "$trusted_marker"
chmod 755 "$trusted" "$trusted_build"
chmod 644 "$trusted_lock" "$trusted_marker"
SWHKD_TRUSTED_DIR="$trusted_dir"
SWHKD_TRUSTED_HELPER="$trusted"
SWHKD_TRUSTED_BUILD_SCRIPT="$trusted_build"
SWHKD_TRUSTED_PINNED_LOCK="$trusted_lock"
SWHKD_TRUSTED_MARKER="$trusted_marker"
STUB_ROOT_DIR="$trusted_dir"
STUB_PARENT="$WORK"
STUB_MODE="644"
DISTRO_FAMILY="debian"
: > "$RECORD"
if install_swhkd_via_trusted_helper; then
    bad "a root-owned non-executable helper must not be used"
else
    ok "a root-owned non-executable helper is refused"
fi
if [[ -s "$RECORD" ]]; then
    bad "a root-owned non-executable helper must receive no privileged command"
else
    ok "a root-owned non-executable helper receives no privileged command"
fi

STUB_MODE="755"
: > "$RECORD"
install_swhkd_via_trusted_helper
if grep -qx "as_root $trusted --distro debian" "$RECORD" \
    && ! grep -q 'target/release' "$RECORD"; then
    ok "the fixed root-owned helper is the only privileged swhkd command"
else
    bad "the only privileged swhkd command must be the fixed root-owned helper"
fi
STUB_ROOT_DIR=""
STUB_PARENT=""
: > "$RECORD"

if grep -qF '/usr/bin/swhkd' "$HELPER"; then
    bad "the helper must never install swhkd to /usr/bin"
else
    ok "the helper never installs swhkd to /usr/bin"
fi

if grep -qF 'SWHKD_MANAGED_DIR="/usr/local/libexec/linux-soundboard"' "$HELPER" \
    && grep -qF '"$SWHKD_MANAGED_BIN"' "$HELPER"; then
    ok "the helper installs to the managed path"
else
    bad "the helper must install to the managed path"
fi

helper_parent_accepts() (
        source "$HELPER"
    managed_parent_is_safe "$1"
)

mkdir -p "$WORK/helper-parent-writable"
if helper_parent_accepts "$WORK/helper-parent-writable/managed" >/dev/null 2>&1; then
    bad "the helper must reject a user-writable managed parent"
else
    ok "the helper rejects a user-writable managed parent"
fi

ln -sfn "$WORK/helper-parent-absent" "$WORK/helper-parent-dangling"
if helper_parent_accepts "$WORK/helper-parent-dangling/managed" >/dev/null 2>&1; then
    bad "the helper must reject a dangling-symlink managed parent"
else
    ok "the helper rejects a dangling-symlink managed parent"
fi

printf 'not a directory\n' > "$WORK/helper-parent-file"
if helper_parent_accepts "$WORK/helper-parent-file/managed" >/dev/null 2>&1; then
    bad "the helper must reject a regular file as managed parent"
else
    ok "the helper rejects a regular file as managed parent"
fi

rm -rf "$WORK/helper-parent-missing"
if helper_parent_accepts "$WORK/helper-parent-missing/managed" >/dev/null 2>&1 \
    && [[ ! -e "$WORK/helper-parent-missing" ]]; then
    ok "the helper accepts an absent parent without creating it during validation"
else
    bad "the helper must accept an absent parent only as a validation result"
fi

parent_check_line="$(grep -n 'managed_parent_is_safe "\$SWHKD_MANAGED_DIR"' "$HELPER" | head -n1 | cut -d: -f1)"
create_line="$(grep -n 'install -d -m755 "\$SWHKD_MANAGED_DIR"' "$HELPER" | head -n1 | cut -d: -f1)"
if [[ -n "$parent_check_line" && -n "$create_line" && "$parent_check_line" -lt "$create_line" ]]; then
    ok "the helper validates an absent parent before creating the managed directory"
else
    bad "the helper must validate the parent before install -d"
fi

if grep -qF 'for pkg in git make rust pkgconf systemd gcc; do' "$HELPER" \
    && grep -qF 'command -v cargo >/dev/null 2>&1' "$HELPER"; then
    ok "the Arch helper treats cargo as part of the rust toolchain instead of a separate package"
else
    bad "the Arch helper must not require a nonexistent standalone cargo package"
fi

if grep -qF 'zypper --non-interactive install git make gcc cargo rust pkgconf-pkg-config systemd-devel' "$HELPER"; then
    ok "the openSUSE helper uses the current pkgconf-pkg-config package name"
else
    bad "the openSUSE helper must use pkgconf-pkg-config"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
