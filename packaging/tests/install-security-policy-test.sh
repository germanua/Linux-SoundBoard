#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); printf 'ok   - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'FAIL - %s\n' "$1"; }

package_hooks=(
    "$REPO_ROOT/packaging/aur/linux-soundboard.install"
    "$REPO_ROOT/packaging/aur/linux-soundboard-git/linux-soundboard-git.install"
    "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"
)

if grep -En '(/usr/bin/swhkd|chmod[[:space:]]+u\+s.*swhkd|chown[[:space:]]+root:root.*swhkd)' "${package_hooks[@]}"; then
    bad "package hooks must not promote third-party swhkd"
else
    ok "package hooks never promote third-party swhkd"
fi

arch_guidance=(
    "$REPO_ROOT/install.sh"
    "$REPO_ROOT/README.md"
    "$REPO_ROOT/docs/INSTALL.md"
    "$REPO_ROOT/docs/TROUBLESHOOTING.md"
    "$REPO_ROOT/src/app/hotkeys/swhkd_install.rs"
    "$REPO_ROOT/packaging/linux/appimage-preflight-check.sh"
    "$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"
)

if grep -En '(^|[^[:alnum:]_])pacman[[:space:]]+-S([[:space:]]|$)' "${arch_guidance[@]}"; then
    bad "Arch guidance must not perform partial pacman installs"
else
    ok "Arch guidance avoids partial pacman installs"
fi

if grep -Fq 'sudo pacman -Syu --needed' "$REPO_ROOT/src/app/hotkeys/swhkd_install.rs" \
    && grep -Fq 'sudo pacman -Syu --needed' "$REPO_ROOT/docs/INSTALL.md"; then
    ok "Arch guidance uses a full system upgrade transaction"
else
    bad "Arch guidance must use pacman -Syu"
fi

for pkgbuild in \
    "$REPO_ROOT/packaging/aur/PKGBUILD" \
    "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD"; do
    if grep -Fq 'build-swhkd-locked.sh' "$pkgbuild" \
        && grep -Fq 'swhkd-Cargo.lock.pinned' "$pkgbuild"; then
        ok "$(basename "$(dirname "$pkgbuild")") ships pinned swhkd inputs"
    else
        bad "$pkgbuild must ship pinned swhkd inputs"
    fi
done

for srcinfo in \
    "$REPO_ROOT/packaging/aur/.SRCINFO" \
    "$REPO_ROOT/packaging/aur/linux-soundboard-git/.SRCINFO"; do
    if grep -Fq 'optdepends = swhkd-git' "$srcinfo"; then
        bad "$srcinfo must not recommend an unpinned swhkd package"
    else
        ok "$(basename "$(dirname "$srcinfo")") does not recommend external swhkd"
    fi
done

if grep -En '/usr/bin/swhks|chmod[[:space:]]+.*swhks' "${package_hooks[@]}"; then
    bad "package hooks must not mutate third-party swhks"
else
    ok "package hooks leave third-party swhks untouched"
fi

if grep -Fq 'provision_trusted_swhkd_helper_from_verified_appimage "$image"' "$REPO_ROOT/install.sh" \
    && grep -Fq 'root_hash="$(root_sha256_of "$root_image")"' "$REPO_ROOT/install.sh" \
    && grep -Fq '"${root_hash,,}" == "$expected"' "$REPO_ROOT/install.sh"; then
    ok "one-command Wayland setup provisions only from a post-copy verified AppImage"
else
    bad "one-command Wayland setup must verify the root-owned AppImage copy before provisioning"
fi

if grep -Fq '"$INSTALL_ROOT/install-user.sh"' "$REPO_ROOT/install.sh" \
    && grep -Fq 'INSTALL_LIFECYCLE_SCRIPT="$INSTALL_ROOT/install-user.sh"' "$REPO_ROOT/packaging/linux/install-user.sh" \
    && grep -Fq 'install_file_from_source "$SCRIPT_DIR/install-user.sh" "$INSTALL_LIFECYCLE_SCRIPT" 755' "$REPO_ROOT/packaging/linux/install-user.sh"; then
    ok "installed AppImage keeps a local lifecycle installer for repair/status/uninstall"
else
    bad "AppImage installs must persist the lifecycle installer"
fi

if awk '/run_user_installer_from_available_source\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'download_appimage' \
    && ! awk '/run_user_installer_from_available_source\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'download_and_extract_tarball'; then
    ok "lifecycle fallback uses a verified AppImage instead of the retired tarball"
else
    bad "lifecycle fallback must use the signed AppImage path"
fi

if awk '/repair_main\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'ensure_trusted_swhkd_helper'; then
    ok "repair restores the trusted helper before repairing Wayland hotkeys"
else
    bad "repair must restore the trusted helper when needed"
fi

if awk '/remove_installation\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'remove_managed_swhkd_assets'; then
    ok "uninstall removes Linux Soundboard managed privileged assets"
else
    bad "uninstall must clean managed privileged assets"
fi

if awk '/remove_managed_swhkd_assets\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'managed_marker_is_safe' \
    && grep -Fq 'SWHKD_MANAGED_MARKER=' "$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"; then
    ok "privileged cleanup is ownership-marker gated"
else
    bad "privileged cleanup must require Linux Soundboard ownership markers"
fi

if grep -Fq 'resolved_app_binary()' "$REPO_ROOT/install.sh" \
    && awk '/collect_app_report\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'resolved_app_binary' \
    && awk '/collect_debug_run\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'resolved_app_binary'; then
    ok "bug reports work with the AppImage user install even when it is not on PATH"
else
    bad "report generation must resolve the managed AppImage binary"
fi

if awk '/print_status\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'print_swhkd_security_status' \
    && awk '/collect_app_report\(\)/,/^}/' "$REPO_ROOT/install.sh" | grep -Fq 'print_swhkd_security_status'; then
    ok "status and reports expose trusted-helper and managed-daemon state"
else
    bad "status/report must include Wayland privilege state"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
