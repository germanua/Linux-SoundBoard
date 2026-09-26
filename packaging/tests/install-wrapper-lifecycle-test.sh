#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_ROOT="$(mktemp -d)"
ORIGINAL_WORK=""
cleanup() {
    [[ -n "$ORIGINAL_WORK" ]] && rm -rf "$ORIGINAL_WORK"
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

sed '/^main "\$@"$/d' "$REPO_ROOT/install.sh" > "$TEST_ROOT/install-functions.sh"
source "$TEST_ROOT/install-functions.sh"
ORIGINAL_WORK="$WORK_DIR"
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok   - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'FAIL - %s\n' "$1"; }

EVIL_PATH="$TEST_ROOT/evil-path"
mkdir -p "$EVIL_PATH"
printf '#!/usr/bin/env bash\nprintf evil\n' > "$EVIL_PATH/install"
chmod +x "$EVIL_PATH/install"
if PATH="$EVIL_PATH:$PATH" trusted_system_command install | grep -q '^/usr/'; then
    ok "privileged command resolution ignores a hostile caller PATH"
else
    bad "privileged command resolution used a hostile caller PATH"
    exit 1
fi
if [[ "$(normalize_release_version v2.4.5)" == "2.4.5" ]]; then
    ok "release tags are normalized before persistence"
else
    bad "release tag normalization failed"
    exit 1
fi

SYSTEM_ROOT="$TEST_ROOT/system"
SWHKD_TRUSTED_DIR="$SYSTEM_ROOT/usr/libexec/linux-soundboard"
SWHKD_TRUSTED_HELPER="$SWHKD_TRUSTED_DIR/install-swhkd-helper.sh"
SWHKD_TRUSTED_BUILD_SCRIPT="$SWHKD_TRUSTED_DIR/build-swhkd-locked.sh"
SWHKD_TRUSTED_PINNED_LOCK="$SWHKD_TRUSTED_DIR/swhkd-Cargo.lock.pinned"
SWHKD_TRUSTED_MARKER="$SWHKD_TRUSTED_DIR/.managed-by-linux-soundboard"
SWHKD_MANAGED_DIR="$SYSTEM_ROOT/usr/local/libexec/linux-soundboard"
SWHKD_MANAGED_BIN="$SWHKD_MANAGED_DIR/swhkd"
SWHKS_MANAGED_BIN="$SWHKD_MANAGED_DIR/swhks"
SWHKD_MANAGED_MARKER="$SWHKD_MANAGED_DIR/.managed-by-linux-soundboard"
mkdir -p "$SYSTEM_ROOT/usr/libexec" "$SYSTEM_ROOT/usr/local/libexec"

as_root() {
    if [[ "$1" == "install" ]]; then
        shift
        local args=()
        while (($# > 0)); do
            case "$1" in
                -o|-g) shift 2 ;;
                *) args+=("$1"); shift ;;
            esac
        done
        command install "${args[@]}"
    else
        "$@"
    fi
}
swhkd_root_owned_directory_is_safe() {
    [[ -d "$1" && ! -L "$1" ]]
}

swhkd_directory_is_privileged_safe() {
    swhkd_root_owned_directory_is_safe "$1" \
        && swhkd_root_owned_directory_is_safe "$(dirname -- "$1")"
}

root_owned_regular_file_is_safe() {
    local path=$1 executable=${2:-0}
    [[ -f "$path" && ! -L "$path" ]] || return 1
    (( executable == 0 )) || [[ -x "$path" ]]
}

swhkd_trusted_helper_is_safe() {
    root_owned_regular_file_is_safe "$1" 1 \
        && swhkd_directory_is_privileged_safe "$(dirname -- "$1")"
}

FAKE_IMAGE="$TEST_ROOT/fake.AppImage"
cat > "$FAKE_IMAGE" <<EOF
#!/usr/bin/env bash
set -euo pipefail
[[ "\${1:-}" == "--appimage-extract" ]]
mkdir -p squashfs-root/usr/libexec/linux-soundboard
cp "$REPO_ROOT/packaging/linux/install-swhkd-helper.sh" squashfs-root/usr/libexec/linux-soundboard/
cp "$REPO_ROOT/packaging/linux/build-swhkd-locked.sh" squashfs-root/usr/libexec/linux-soundboard/
cp "$REPO_ROOT/packaging/linux/swhkd-Cargo.lock.pinned" squashfs-root/usr/libexec/linux-soundboard/
EOF
chmod +x "$FAKE_IMAGE"
IMAGE_HASH="$(sha256sum "$FAKE_IMAGE" | awk '{print $1}')"
if provision_trusted_swhkd_helper_from_verified_appimage "$FAKE_IMAGE" "$IMAGE_HASH"; then
    if [[ -x "$SWHKD_TRUSTED_HELPER" \
        && -x "$SWHKD_TRUSTED_BUILD_SCRIPT" \
        && -f "$SWHKD_TRUSTED_PINNED_LOCK" \
        && -f "$SWHKD_TRUSTED_MARKER" ]]; then
        ok "verified AppImage provisioning installs the fixed trusted helper bundle"
    else
        bad "trusted helper bundle is incomplete after provisioning"
    fi
else
    bad "verified AppImage provisioning should succeed"
fi

HELPER_BEFORE="$(sha256sum "$SWHKD_TRUSTED_HELPER" | awk '{print $1}')"
printf '\nchanged\n' >> "$FAKE_IMAGE"
if provision_trusted_swhkd_helper_from_verified_appimage "$FAKE_IMAGE" "$IMAGE_HASH"; then
    bad "a changed AppImage must fail the root-copy checksum"
else
    HELPER_AFTER="$(sha256sum "$SWHKD_TRUSTED_HELPER" | awk '{print $1}')"
    if [[ "$HELPER_BEFORE" == "$HELPER_AFTER" ]]; then
        ok "a changed AppImage is rejected before trusted assets change"
    else
        bad "checksum rejection must leave trusted assets unchanged"
    fi
fi
mkdir -p "$SWHKD_MANAGED_DIR"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SWHKD_MANAGED_BIN"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SWHKS_MANAGED_BIN"
printf 'managed-by: linux-soundboard\n' > "$SWHKD_MANAGED_MARKER"
chmod +x "$SWHKD_MANAGED_BIN" "$SWHKS_MANAGED_BIN"

remove_managed_swhkd_assets
if [[ ! -e "$SWHKD_MANAGED_DIR" && ! -e "$SWHKD_TRUSTED_DIR" ]]; then
    ok "uninstall cleanup removes ownership-marked privileged assets"
else
    bad "ownership-marked privileged assets should be removed"
fi

mkdir -p "$SWHKD_MANAGED_DIR"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SWHKD_MANAGED_BIN"
chmod +x "$SWHKD_MANAGED_BIN"
remove_managed_swhkd_assets
if [[ -e "$SWHKD_MANAGED_BIN" ]]; then
    ok "uninstall cleanup leaves an unmarked privileged directory untouched"
else
    bad "unmarked privileged assets must not be deleted"
fi
rm -rf "$SWHKD_MANAGED_DIR"
ORDER="$TEST_ROOT/repair-order"
detect_distro() { :; }
detect_session() { SESSION_TYPE="wayland"; }
is_wayland() { return 0; }
installed_native_packages() { return 1; }
run_user_installer_from_available_source() { printf 'lifecycle\n' >> "$ORDER"; }
ensure_trusted_swhkd_helper() { printf 'helper\n' >> "$ORDER"; }
repair_swhkd_if_needed() { printf 'swhkd\n' >> "$ORDER"; }
ensure_pipewire_services() { printf 'pipewire\n' >> "$ORDER"; }
repair_main
if [[ "$(tr '\n' ' ' < "$ORDER")" == "lifecycle helper swhkd pipewire " ]]; then
    ok "repair restores the trusted helper before repairing Wayland hotkeys"
else
    bad "repair lifecycle order is incorrect"
fi

APP_BINARY="linux-soundboard-test"
INSTALL_ROOT="$TEST_ROOT/user-install"
mkdir -p "$INSTALL_ROOT"
printf '#!/usr/bin/env bash\nexit 0\n' > "$INSTALL_ROOT/$APP_BINARY"
chmod +x "$INSTALL_ROOT/$APP_BINARY"
if [[ "$(resolved_app_binary)" == "$INSTALL_ROOT/$APP_BINARY" ]]; then
    ok "report binary resolution finds a managed AppImage install outside PATH"
else
    bad "report binary resolution must find the managed AppImage install"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
