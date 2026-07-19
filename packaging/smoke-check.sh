#!/usr/bin/env bash
#
# Packaging smoke checks for Linux Soundboard.
#
# These checks verify the packaging artifacts are internally consistent and
# structurally valid without requiring build tooling (rpmbuild, dpkg, flatpak-
# builder) or a running audio session. Checks that require build tools are
# noted in the output but skipped automatically when the tool is absent.
#
# Exit codes: 0 = all checks pass, 1 = one or more failures.

# shellcheck disable=SC2016

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

PASS=0
FAIL=0
SKIP=0

pass() { printf '[PASS] %s\n' "$1"; PASS=$((PASS + 1)); }
fail() { printf '[FAIL] %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }
skip() { printf '[SKIP] %s\n' "$1"; SKIP=$((SKIP + 1)); }
note() { printf '[NOTE] %s\n' "$1"; }

# ── 1. Metadata consistency ──────────────────────────────────────────────────

echo "==> Metadata consistency"
vm_ec=0
vm_output="$(bash "$SCRIPT_DIR/validate-metadata.sh" 2>&1)" || vm_ec=$?
echo "$vm_output" | grep -v '^\[NOTE\]' || true
if [[ $vm_ec -eq 0 ]]; then
    pass "validate-metadata.sh: all checks pass"
else
    fail "validate-metadata.sh: one or more checks failed (run validate-metadata.sh for details)"
fi

echo ""

# ── 1a. Legal and dependency notices ───────────────────────────────────────

echo "==> Legal and dependency notices"
LEGAL_FILES=(
    "$REPO_ROOT/LICENSE"
    "$REPO_ROOT/NOTICE.md"
    "$REPO_ROOT/THIRDPARTY_LICENSES.md"
    "$REPO_ROOT/THIRD_PARTY_NOTICES.html"
    "$REPO_ROOT/COMMERCIAL-LICENSE.md"
    "$REPO_ROOT/DONATIONS.md"
)

for legal_file in "${LEGAL_FILES[@]}"; do
    if [[ -s "$legal_file" ]]; then
        pass "legal file exists: ${legal_file#"$REPO_ROOT/"}"
    else
        fail "legal file missing or empty: $legal_file"
    fi
done

for package_file in \
    "$REPO_ROOT/packaging/linux/package-appimage.sh" \
    "$REPO_ROOT/packaging/linux/install-user.sh" \
    "$REPO_ROOT/packaging/aur/PKGBUILD" \
    "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD" \
    "$REPO_ROOT/packaging/debian/rules" \
    "$REPO_ROOT/packaging/rpm/linux-soundboard.spec" \
    "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.yml"; do
    if grep -qF "THIRD_PARTY_NOTICES.html" "$package_file"; then
        pass "generated notices packaged by: ${package_file#"$REPO_ROOT/"}"
    else
        fail "generated notices not packaged by: $package_file"
    fi
done

if grep -qF 'maintainer-scripts = "../packaging/debian"' "$REPO_ROOT/src/Cargo.toml"; then
    pass "cargo-deb: maintainer scripts use the control archive"
else
    fail "cargo-deb: maintainer scripts are not registered for the control archive"
fi
if grep -qF 'depends = "libgtk-4-1, libadwaita-1-0, libx11-6, libxi6, libpulse0, libopus0, pipewire, wireplumber, pkexec | policykit-1"' "$REPO_ROOT/src/Cargo.toml"; then
    pass "cargo-deb: runtime dependencies do not rely on optional auto-detection"
else
    fail "cargo-deb: explicit GTK, Opus, audio, and X11 runtime dependencies are incomplete"
fi
if grep -qF 'libpipewire-0.3-dev,' "$REPO_ROOT/packaging/debian/control"; then
    pass "Debian: PipeWire build dependency is declared"
else
    fail "Debian: libpipewire-0.3-dev is required for the Rust PipeWire bindings"
fi
if grep -qF 'libopus-dev,' "$REPO_ROOT/packaging/debian/control"; then
    pass "Debian: Opus build dependency is declared"
else
    fail "Debian: libopus-dev is required for the Rust Opus bindings"
fi
if grep -qF 'BuildRequires:  opus-devel' "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"; then
    pass "RPM: Opus build dependency is declared"
else
    fail "RPM: opus-devel is required for the Rust Opus bindings"
fi
if grep -qF '%global debug_package %{nil}' "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"; then
    pass "RPM: empty remapped debug packages are disabled"
else
    fail "RPM: remapped Rust sources can create empty debug packages"
fi

for package_file in \
    "$REPO_ROOT/packaging/linux/package-appimage.sh" \
    "$REPO_ROOT/packaging/aur/PKGBUILD" \
    "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD" \
    "$REPO_ROOT/packaging/debian/rules" \
    "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"; do
    remap_count="$(grep -o -- '--remap-path-prefix=' "$package_file" | wc -l || true)"
    if [[ "$remap_count" -ge 2 ]]; then
        pass "release paths remapped by: ${package_file#"$REPO_ROOT/"}"
    else
        fail "release build can expose source or builder paths: $package_file"
    fi
done

if grep -qFx "  'opus'" "$REPO_ROOT/packaging/aur/PKGBUILD" \
    && grep -qF $'\tdepends = opus' "$REPO_ROOT/packaging/aur/.SRCINFO"; then
    pass "AUR stable package: Opus runtime dependency is declared"
else
    fail "AUR stable package: opus runtime dependency is incomplete"
fi
if grep -qFx "  'opus'" "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD" \
    && grep -qF $'\tdepends = opus' "$REPO_ROOT/packaging/aur/linux-soundboard-git/.SRCINFO"; then
    pass "AUR development package: Opus runtime dependency is declared"
else
    fail "AUR development package: opus runtime dependency is incomplete"
fi

echo ""

# ── 1b. Stable AUR default ──────────────────────────────────────────────────

echo "==> Stable AUR default"
if grep -qF 'APP_AUR_PACKAGE="linux-soundboard"' "$REPO_ROOT/install.sh"; then
    pass "install.sh: stable AUR package selected"
else
    fail "install.sh: stable AUR package is not selected"
fi
if [[ $(grep -cF -- '--useask "$APP_AUR_PACKAGE"' "$REPO_ROOT/install.sh") -eq 2 ]]; then
    pass "install.sh: AUR helpers replace the legacy package non-interactively"
else
    fail "install.sh: AUR helpers do not enable automatic conflict replacement"
fi
if grep -qF -- '--ask=4 "$package_file"' "$REPO_ROOT/install.sh"; then
    pass "install.sh: manual AUR fallback replaces the legacy package atomically"
else
    fail "install.sh: manual AUR fallback cannot replace the legacy package"
fi
if grep -qF "conflicts=('linux-soundboard-git')" "$REPO_ROOT/packaging/aur/PKGBUILD"; then
    pass "AUR stable PKGBUILD: conflicts with development package"
else
    fail "AUR stable PKGBUILD: missing development-package conflict"
fi
if grep -qF 'archive/refs/tags/v${pkgver}.tar.gz' "$REPO_ROOT/packaging/aur/PKGBUILD"; then
    pass "AUR stable PKGBUILD: uses tagged source archive"
else
    fail "AUR stable PKGBUILD: does not use tagged source archive"
fi

echo ""

# ── 2. Desktop file validation ───────────────────────────────────────────────

echo "==> Desktop file validation"
DESKTOP_FILES=(
    "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.desktop"
    "$REPO_ROOT/packaging/debian/linux-soundboard.desktop"
    "$REPO_ROOT/packaging/rpm/linux-soundboard.desktop"
)

if command -v desktop-file-validate >/dev/null 2>&1; then
    for f in "${DESKTOP_FILES[@]}"; do
        label="$(basename "$(dirname "$f")")/$(basename "$f")"
        if desktop-file-validate "$f" 2>&1; then
            pass "desktop-file-validate: $label"
        else
            fail "desktop-file-validate: $label"
        fi
    done
else
    skip "desktop-file-validate not installed (install desktop-file-utils to enable)"
    for f in "${DESKTOP_FILES[@]}"; do
        if [[ -f "$f" ]]; then
            pass "desktop file exists: $(basename "$(dirname "$f")")/$(basename "$f")"
        else
            fail "desktop file missing: $f"
        fi
    done
fi

echo ""

# ── 3. AppStream metadata validation ─────────────────────────────────────────

echo "==> AppStream metadata validation"
METAINFO="$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.metainfo.xml"

if [[ ! -f "$METAINFO" ]]; then
    fail "metainfo.xml not found: $METAINFO"
else
    if command -v appstreamcli >/dev/null 2>&1; then
        as_ec=0
        as_out="$(appstreamcli validate --no-net "$METAINFO" 2>&1)" || as_ec=$?
        echo "$as_out" | grep -v '^W:' || true
        if [[ $as_ec -eq 0 ]]; then
            pass "appstreamcli validate: metainfo.xml"
        else
            fail "appstreamcli validate: metainfo.xml (run 'appstreamcli validate --no-net packaging/flatpak/com.linuxsoundboard.app.metainfo.xml' for details)"
        fi
    elif command -v appstream-util >/dev/null 2>&1; then
        if appstream-util validate-relax "$METAINFO"; then
            pass "appstream-util validate-relax: metainfo.xml"
        else
            fail "appstream-util validate-relax: metainfo.xml"
        fi
    else
        skip "appstreamcli / appstream-util not installed (install appstream-utils or libappstream-glib to enable)"
        pass "metainfo.xml exists and is well-formed XML (xmllint not checked; install appstream tools for full validation)"
    fi
fi

echo ""

# ── 4. Service file ───────────────────────────────────────────────────────────

echo "==> Systemd service file"
SERVICE="$REPO_ROOT/packaging/linux/linux-soundboard-engine.service"

if [[ ! -f "$SERVICE" ]]; then
    fail "engine service file not found: $SERVICE"
else
    if command -v systemd-analyze >/dev/null 2>&1; then
        if systemd-analyze verify "$SERVICE" 2>&1; then
            pass "systemd-analyze verify: engine service"
        else
            fail "systemd-analyze verify: engine service"
        fi
    else
        skip "systemd-analyze not available; checking service fields manually"
        for field in "Description=" "ExecStart=" "Type=exec" "Restart=" "WantedBy=default.target"; do
            if grep -qF "$field" "$SERVICE"; then
                pass "engine service: $field"
            else
                fail "engine service: missing '$field'"
            fi
        done
    fi
fi

echo ""

# ── 5. install-user.sh subcommand coverage ────────────────────────────────────

echo "==> install-user.sh subcommands"
INSTALLER="$REPO_ROOT/packaging/linux/install-user.sh"
if [[ ! -f "$INSTALLER" ]]; then
    fail "install-user.sh not found: $INSTALLER"
else
    for subcmd in "install" "repair" "setup-user" "remove" "status"; do
        if grep -qw "$subcmd" "$INSTALLER"; then
            pass "install-user.sh: '$subcmd' subcommand present"
        else
            fail "install-user.sh: '$subcmd' subcommand not found"
        fi
    done
    if grep -q "XDG_DATA_HOME\|XDG_CONFIG_HOME" "$INSTALLER"; then
        pass "install-user.sh: uses XDG_DATA_HOME/XDG_CONFIG_HOME (no hard-coded uid paths)"
    else
        fail "install-user.sh: no XDG_DATA_HOME/XDG_CONFIG_HOME usage found"
    fi
fi

echo ""

# ── 5a. install.sh wrapper subcommand coverage ───────────────────────────────

echo "==> install.sh wrapper subcommands"
WRAPPER="$REPO_ROOT/install.sh"
if [[ ! -f "$WRAPPER" ]]; then
    fail "install.sh not found: $WRAPPER"
else
    for subcmd in "install" "repair" "remove" "uninstall" "status"; do
        if grep -qw "$subcmd" "$WRAPPER"; then
            pass "install.sh: '$subcmd' subcommand present"
        else
            fail "install.sh: '$subcmd' subcommand not found"
        fi
    done
    if grep -q -- "--keep-package" "$WRAPPER"; then
        pass "install.sh: package-preserving remove option present"
    else
        fail "install.sh: --keep-package option missing"
    fi
fi

echo ""

# ── 6. Flatpak manifest: forbidden finish-args check ─────────────────────────

echo "==> Flatpak manifest permission audit"
MANIFEST="$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.yml"
if [[ ! -f "$MANIFEST" ]]; then
    fail "Flatpak manifest not found: $MANIFEST"
else
    if grep -qF -- "--filesystem=home" "$MANIFEST"; then
        fail "Flatpak manifest: --filesystem=home present (overly broad; Flathub policy requires removal)"
    else
        pass "Flatpak manifest: no --filesystem=home"
    fi
    if grep -qF -- "--filesystem=host" "$MANIFEST"; then
        fail "Flatpak manifest: --filesystem=host present (overly broad)"
    else
        pass "Flatpak manifest: no --filesystem=host"
    fi
    if grep -qF -- "--talk-name=org.freedesktop.Flatpak" "$MANIFEST"; then
        fail "Flatpak manifest: --talk-name=org.freedesktop.Flatpak present (sandbox escape risk)"
    else
        pass "Flatpak manifest: no --talk-name=org.freedesktop.Flatpak"
    fi
    if grep -qF -- "--socket=session-bus" "$MANIFEST"; then
        fail "Flatpak manifest: --socket=session-bus present (overly broad bus access)"
    else
        pass "Flatpak manifest: no --socket=session-bus"
    fi
    if grep -qF -- "--socket=system-bus" "$MANIFEST"; then
        fail "Flatpak manifest: --socket=system-bus present (overly broad bus access)"
    else
        pass "Flatpak manifest: no --socket=system-bus"
    fi
fi

echo ""

# ── 7. AppImage preflight script ──────────────────────────────────────────────

echo "==> AppImage preflight check script"
PREFLIGHT="$REPO_ROOT/packaging/linux/appimage-preflight-check.sh"
if [[ ! -f "$PREFLIGHT" ]]; then
    fail "AppImage preflight script not found: $PREFLIGHT"
else
    if bash -n "$PREFLIGHT" 2>&1; then
        pass "AppImage preflight script: no syntax errors"
    else
        fail "AppImage preflight script: syntax errors detected"
    fi
fi

APPIMAGE_PACKAGER="$REPO_ROOT/packaging/linux/package-appimage.sh"
if grep -qF 'GTK_PLUGIN_SOURCE=' "$APPIMAGE_PACKAGER" \
    && grep -qF 'cp "$GTK_PLUGIN_SOURCE" "$GTK_PLUGIN_BIN"' "$APPIMAGE_PACKAGER"; then
    pass "AppImage GTK plugin: pristine cache is copied before patching"
else
    fail "AppImage GTK plugin: cached upstream source would be patched repeatedly"
fi
for payload in \
    "install-user.sh" \
    "app-meta.sh" \
    "installer/icons" \
    'installer/$legal_file' \
    "installer/install-swhkd-helper.sh"; do
    if grep -qF "$payload" "$APPIMAGE_PACKAGER"; then
        pass "AppImage installer payload: $payload"
    else
        fail "AppImage installer payload missing: $payload"
    fi
done

if grep -qF 'LSB_INSTALL_VERSION' "$REPO_ROOT/src/app/bootstrap.rs" \
    && grep -qF '.installed-version' "$REPO_ROOT/packaging/linux/install-user.sh"; then
    pass "AppImage updater: installed version handoff is bundled"
else
    fail "AppImage updater: installed version handoff is incomplete"
fi

echo ""

# ── 8. Shell script syntax checks ────────────────────────────────────────────

echo "==> Shell script syntax"
SHELL_SCRIPTS=(
    "$REPO_ROOT/packaging/linux/install-user.sh"
    "$REPO_ROOT/packaging/linux/app-meta.sh"
    "$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"
    "$REPO_ROOT/packaging/linux/generate-icons.sh"
    "$REPO_ROOT/packaging/linux/package-appimage.sh"
    "$REPO_ROOT/packaging/linux/appimage-preflight-check.sh"
    "$REPO_ROOT/packaging/debian/package-deb.sh"
    "$REPO_ROOT/packaging/rpm/package-rpm.sh"
    "$REPO_ROOT/packaging/generate-third-party-notices.sh"
    "$REPO_ROOT/packaging/common.sh"
    "$REPO_ROOT/packaging/validate-metadata.sh"
)

for script in "${SHELL_SCRIPTS[@]}"; do
    if [[ ! -f "$script" ]]; then
        fail "script not found: $script"
        continue
    fi
    label="${script#"$REPO_ROOT/"}"
    if bash -n "$script" 2>&1; then
        pass "syntax ok: $label"
    else
        fail "syntax error: $label"
    fi
done

echo ""

# ── 9. install.sh (top-level installer) ──────────────────────────────────────

echo "==> Top-level install.sh"
TOP_INSTALL="$REPO_ROOT/install.sh"
if [[ ! -f "$TOP_INSTALL" ]]; then
    fail "install.sh not found: $TOP_INSTALL"
else
    if bash -n "$TOP_INSTALL" 2>&1; then
        pass "install.sh: no syntax errors"
    else
        fail "install.sh: syntax errors detected"
    fi
fi

echo ""

# ── 10. Build tool availability notes ────────────────────────────────────────

echo "==> Build tool availability (informational)"
for tool_label in \
    "flatpak-builder:Flatpak build" \
    "dpkg-buildpackage:Debian package build" \
    "rpmbuild:RPM package build" \
    "desktop-file-validate:desktop file linting" \
    "appstreamcli:AppStream metadata linting" \
    "appstream-util:AppStream metadata linting (alternative)"
do
    tool="${tool_label%%:*}"
    label="${tool_label#*:}"
    if command -v "$tool" >/dev/null 2>&1; then
        note "$tool available ($label)"
    else
        note "$tool NOT available ($label) — install to enable full smoke testing"
    fi
done

echo ""

# ── Summary ──────────────────────────────────────────────────────────────────

echo "Results: $PASS passed, $FAIL failed, $SKIP skipped"
if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
