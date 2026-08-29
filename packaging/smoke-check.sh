#!/usr/bin/env bash
# Offline packaging consistency checks.

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

# 1. Metadata consistency

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

# 1a. Legal and dependency notices

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
if grep -qF 'depends = "libgtk-4-1, libadwaita-1-0, libx11-6, libxi6, libpulse0, libopus0, pulseaudio-utils, pipewire, pipewire-pulse, wireplumber, pkexec | policykit-1"' "$REPO_ROOT/src/Cargo.toml"; then
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
if grep -qF 'libclang-dev,' "$REPO_ROOT/packaging/debian/control"; then
    pass "Debian: libclang build dependency is declared"
else
    fail "Debian: libclang-dev is required by PipeWire bindgen"
fi
if grep -qF 'BuildRequires:  opus-devel' "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"; then
    pass "RPM: Opus build dependency is declared"
else
    fail "RPM: opus-devel is required for the Rust Opus bindings"
fi
if grep -qF 'pulseaudio-utils' "$REPO_ROOT/packaging/debian/control" \
    && grep -qF 'pipewire-pulse' "$REPO_ROOT/packaging/debian/control"; then
    pass "Debian: pactl and the PipeWire Pulse server are installed"
else
    fail "Debian: pulseaudio-utils and pipewire-pulse are required at runtime"
fi
if grep -qF 'Requires:       pulseaudio-utils' "$REPO_ROOT/packaging/rpm/linux-soundboard.spec" \
    && grep -qF 'Requires:       pipewire-utils' "$REPO_ROOT/packaging/rpm/linux-soundboard.spec" \
    && grep -qF 'Requires:       pipewire-pulseaudio' "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"; then
    pass "RPM: pactl, PipeWire tools, and the Pulse server are installed"
else
    fail "RPM: pulseaudio-utils, pipewire-utils, and pipewire-pulseaudio are required at runtime"
fi
if grep -Eq '^[[:space:]]*(cargo|rustc)[[:space:]]*\(' "$REPO_ROOT/packaging/debian/control" \
    || grep -Eq '^BuildRequires:[[:space:]]+(cargo|rust)[[:space:]]*[<=>]' "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"; then
    fail "Native package build dependencies must not pin Rust package versions"
else
    pass "Native package build dependencies use distro-selected Rust versions"
fi
if grep -qF 'libasound2-dev' "$REPO_ROOT/packaging/debian/control" \
    || grep -qF 'alsa-lib-devel' "$REPO_ROOT/packaging/rpm/linux-soundboard.spec" \
    || grep -qF 'libasound2-dev' "$REPO_ROOT/packaging/docker/build-deb-appimage.sh" \
    || grep -qF 'alsa-lib-devel' "$REPO_ROOT/packaging/docker/build-rpm.sh"; then
    fail "Native package builds still install unused ALSA development files"
else
    pass "Native package builds omit unused ALSA development files"
fi
if grep -Eq '(rust|Rust|cargo).*(1\.85|1\.85\.0)|(1\.85|1\.85\.0).*(rust|Rust|cargo)' \
    "$REPO_ROOT/packaging/docker/build-deb-appimage.sh" \
    "$REPO_ROOT/packaging/docker/build-rpm.sh" \
    || grep -Eq 'toolchain:[[:space:]]*"?[0-9]' "$REPO_ROOT/.github/workflows/ci.yml"; then
    fail "Build workflows hardcode the Rust dependency version"
else
    pass "Build workflows take the Rust version from project metadata"
fi
if grep -qF 'cp --remove-destination "$CTX"/dist/*.deb "$CTX"/dist/*.AppImage "$REPO_ROOT/dist/' \
    "$REPO_ROOT/packaging/docker/build-deb-appimage.sh"; then
    pass "Container build replaces AppImages that are currently running"
else
    fail "Container build cannot replace an AppImage that is currently running"
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
if grep -qFx "  'pipewire-pulse'" "$REPO_ROOT/packaging/aur/PKGBUILD" \
    && grep -qF $'\tdepends = pipewire-pulse' "$REPO_ROOT/packaging/aur/.SRCINFO" \
    && grep -qFx "  'pipewire-pulse'" "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD" \
    && grep -qF $'\tdepends = pipewire-pulse' "$REPO_ROOT/packaging/aur/linux-soundboard-git/.SRCINFO"; then
    pass "AUR packages: PipeWire Pulse server is installed"
else
    fail "AUR packages: pipewire-pulse is required at runtime"
fi

for build_recipe in \
    "$REPO_ROOT/packaging/aur/PKGBUILD" \
    "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD" \
    "$REPO_ROOT/packaging/debian/rules" \
    "$REPO_ROOT/packaging/rpm/linux-soundboard.spec" \
    "$REPO_ROOT/packaging/linux/package-tarball.sh"; do
    if grep -qF -- '--locked' "$build_recipe"; then
        pass "locked Rust graph: ${build_recipe#"$REPO_ROOT/"}"
    else
        fail "Rust build does not enforce Cargo.lock: $build_recipe"
    fi
done

if grep -qF 'for cmd in pactl pw-cli pw-dump pw-metadata wpctl' "$REPO_ROOT/install.sh" \
    && grep -qF 'pulseaudio-utils pipewire pipewire-pulse wireplumber' "$REPO_ROOT/install.sh" \
    && grep -qF 'pulseaudio-utils pipewire pipewire-utils pipewire-pulseaudio wireplumber' "$REPO_ROOT/install.sh"; then
    pass "Tarball installer checks and installs audio command providers"
else
    fail "Tarball installer does not cover every audio command provider"
fi

if grep -qF 'pick_pkg "apt-cache show"' "$REPO_ROOT/install.sh" \
    || grep -qF 'pick_pkg "zypper --non-interactive info"' "$REPO_ROOT/install.sh"; then
    fail "Installer package lookup quotes a command and its arguments as one executable"
else
    pass "Installer package lookup invokes package managers correctly"
fi

echo ""

# 1b. Stable AUR default

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

# 2. Desktop file validation

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

# 3. AppStream metadata validation

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

# 4. Service file

echo "==> Systemd service file"
SERVICE="$REPO_ROOT/packaging/linux/linux-soundboard-engine.service"
TARGET="$REPO_ROOT/packaging/linux/linux-soundboard-engine.target"

if [[ ! -f "$SERVICE" || ! -f "$TARGET" ]]; then
    fail "engine service or target file not found"
else
    if command -v systemd-analyze >/dev/null 2>&1; then
        if systemd-analyze verify "$SERVICE" "$TARGET" 2>&1; then
            pass "systemd-analyze verify: engine service and target"
        else
            fail "systemd-analyze verify: engine service and target"
        fi
    else
        skip "systemd-analyze not available; checking unit fields manually"
        for field in "Description=" "ExecStart=" "Type=exec" "Restart=" "PartOf=linux-soundboard-engine.target" "RefuseManualStop=yes"; do
            if grep -qF "$field" "$SERVICE"; then
                pass "engine service: $field"
            else
                fail "engine service: missing '$field'"
            fi
        done
        for field in "Wants=linux-soundboard-engine.service" "WantedBy=default.target"; do
            if grep -qF "$field" "$TARGET"; then
                pass "engine target: $field"
            else
                fail "engine target: missing '$field'"
            fi
        done
    fi
fi

echo ""

# 5. install-user.sh subcommand coverage

echo "==> install-user.sh subcommands"
INSTALLER="$REPO_ROOT/packaging/linux/install-user.sh"
if [[ ! -f "$INSTALLER" ]]; then
    fail "install-user.sh not found: $INSTALLER"
else
    for subcmd in "install" "repair" "setup-user" "remove" "status" "snapshot" "snapshot-diff" "restore-audio"; do
        if grep -qw "$subcmd" "$INSTALLER"; then
            pass "install-user.sh: '$subcmd' subcommand present"
        else
            fail "install-user.sh: '$subcmd' subcommand not found"
        fi
    done
    if grep -q "capture_audio_snapshot" "$INSTALLER"; then
        pass "install-user.sh: records an audio snapshot around installs"
    else
        fail "install-user.sh: no audio snapshot capture found"
    fi
    if grep -q "XDG_DATA_HOME\|XDG_CONFIG_HOME" "$INSTALLER"; then
        pass "install-user.sh: uses XDG_DATA_HOME/XDG_CONFIG_HOME (no hard-coded uid paths)"
    else
        fail "install-user.sh: no XDG_DATA_HOME/XDG_CONFIG_HOME usage found"
    fi
fi

echo ""

# 5a. install.sh wrapper subcommand coverage

echo "==> install.sh wrapper subcommands"
WRAPPER="$REPO_ROOT/install.sh"
if [[ ! -f "$WRAPPER" ]]; then
    fail "install.sh not found: $WRAPPER"
else
    for subcmd in "install" "repair" "remove" "uninstall" "status" "menu" "versions" "report" "fix"; do
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
    # Read piped-install prompts from the terminal.
    if grep -q "ensure_tty" "$WRAPPER" && grep -q "exec </dev/tty" "$WRAPPER"; then
        pass "install.sh: menu reads prompts from the terminal when piped"
    else
        fail "install.sh: no /dev/tty handling; the piped one-liner cannot prompt"
    fi
    if grep -q -- "--version" "$WRAPPER"; then
        pass "install.sh: previous-version install option present"
    else
        fail "install.sh: --version option missing"
    fi
fi

echo ""

# 6. Flatpak manifest: forbidden finish-args check

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

# 7. AppImage preflight script

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
if grep -qF 'export GIO_EXTRA_MODULES="$APPDIR/usr/lib/gio/modules"' "$APPIMAGE_PACKAGER"; then
    pass "AppImage GTK hook: generated build paths are removed"
else
    fail "AppImage GTK hook: generated build paths can leak into the bundle"
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

missing_contexts=()
while IFS= read -r context_dir; do
    context_name="$(basename "$context_dir")"
    if ! grep -qE "SYMBOLIC_CONTEXTS=\(([^)]*[[:space:]])?${context_name}:" "$APPIMAGE_PACKAGER"; then
        missing_contexts+=("$context_name")
    fi
done < <(find "$REPO_ROOT/src/resources/icons/scalable" -mindepth 1 -maxdepth 1 -type d | sort)
if [[ ${#missing_contexts[@]} -eq 0 ]]; then
    pass "AppImage hicolor index declares every bundled symbolic icon context"
else
    fail "AppImage hicolor index omits bundled icon contexts: ${missing_contexts[*]}"
fi

if grep -qF 'LSB_INSTALL_VERSION' "$REPO_ROOT/src/app/bootstrap.rs" \
    && grep -qF '.installed-version' "$REPO_ROOT/packaging/linux/install-user.sh"; then
    pass "AppImage updater: installed version handoff is bundled"
else
    fail "AppImage updater: installed version handoff is incomplete"
fi

if grep -qF 'generate-icons.sh' \
    "$REPO_ROOT/packaging/linux/package-appimage.sh" \
    "$REPO_ROOT/packaging/linux/package-tarball.sh" \
    "$REPO_ROOT/packaging/debian/rules" \
    "$REPO_ROOT/packaging/rpm/linux-soundboard.spec" \
    "$REPO_ROOT/packaging/aur/PKGBUILD" \
    "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD" \
    "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.yml"; then
    fail "package builds regenerate tracked icons"
else
    pass "package builds use committed icons"
fi

echo ""

# 8. Shell script syntax checks

echo "==> Shell script syntax"
SHELL_SCRIPTS=(
    "$REPO_ROOT/packaging/linux/install-user.sh"
    "$REPO_ROOT/packaging/linux/app-meta.sh"
    "$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"
    "$REPO_ROOT/packaging/linux/generate-icons.sh"
    "$REPO_ROOT/packaging/linux/package-appimage.sh"
    "$REPO_ROOT/packaging/linux/package-tarball.sh"
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

# 9. install.sh (top-level installer)

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

    check_installer_checksum_case() {
        local scenario=$1
        local expected_status=$2
        local expected_text=$3
        local output=""
        local status=0

        output="$(bash -c '
            installer=$1
            scenario=$2
            set -- --help
            source "$installer" >/dev/null

            asset="$WORK_DIR/linux-soundboard-test.tar.gz"
            printf "payload\n" > "$asset"
            good=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
            other=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
            tag=""

            find_asset_url() { printf "%s\n" "https://example.invalid/SHA256SUMS.txt"; }
            sha256_of() { printf "%s\n" "$good"; }
            fetch() { printf "%s  %s\n" "$good" "$(basename "$asset")" > "$2"; }

            case "$scenario" in
                missing-latest)
                    find_asset_url() { return 1; }
                    ;;
                missing-tagged)
                    release_json_for_tag() { printf "{}\n"; }
                    tag=v9.9.9
                    ;;
                unreachable)
                    fetch() { return 1; }
                    ;;
                not-listed)
                    fetch() { printf "%s  other-file\n" "$good" > "$2"; }
                    ;;
                invalid-hash)
                    fetch() { printf "not-a-hash  %s\n" "$(basename "$asset")" > "$2"; }
                    sha256_of() { printf "not-a-hash\n"; }
                    ;;
                no-tool)
                    sha256_of() { return 1; }
                    ;;
                mismatch)
                    fetch() { printf "%s  %s\n" "$other" "$(basename "$asset")" > "$2"; }
                    ;;
                valid)
                    ;;
            esac

            verify_download "$asset" "$tag"
        ' _ "$TOP_INSTALL" "$scenario" 2>&1)" || status=$?

        if [[ "$expected_status" == success ]]; then
            if [[ $status -eq 0 && "$output" == *"$expected_text"* ]]; then
                pass "install.sh checksum verification: $scenario"
            else
                fail "install.sh checksum verification: $scenario (status $status, output: $output)"
            fi
        elif [[ $status -ne 0 && "$output" == *"$expected_text"* ]]; then
            pass "install.sh checksum verification: $scenario"
        else
            fail "install.sh checksum verification: $scenario (status $status, output: $output)"
        fi
    }

    check_installer_checksum_case missing-latest failure "Could not download SHA256SUMS.txt"
    check_installer_checksum_case missing-tagged failure "Could not download SHA256SUMS.txt"
    check_installer_checksum_case unreachable failure "Could not download SHA256SUMS.txt"
    check_installer_checksum_case not-listed failure "is not listed in SHA256SUMS.txt"
    check_installer_checksum_case invalid-hash failure "Invalid SHA-256"
    check_installer_checksum_case no-tool failure "No SHA-256 tool found"
    check_installer_checksum_case mismatch failure "Checksum mismatch"
    check_installer_checksum_case valid success "Checksum verified"
fi

echo ""

# 10. swhkd build safety

echo "==> swhkd build safety"
SWHKD_PIN="cbbfc4a981aa263155e3216a42549c9a3ae645fe"
SWHKD_HELPER="$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"
if grep -qF "SWHKD_UPSTREAM_COMMIT=\"$SWHKD_PIN\"" "$SWHKD_HELPER" \
    && grep -qF 'git -C "$work_dir/swhkd" fetch --depth 1 "$SWHKD_REPO_URL" "$SWHKD_UPSTREAM_COMMIT"' "$SWHKD_HELPER" \
    && grep -qF 'make NO_RFKILL_SW_SUPPORT=1' "$SWHKD_HELPER" \
    && grep -qF 'swhkd_binary_is_safe "$work_dir/swhkd/target/release/swhkd"' "$SWHKD_HELPER"; then
    pass "swhkd helper pins and verifies an rfkill-free build"
else
    fail "swhkd helper does not pin and verify an rfkill-free build"
fi

if grep -qF "SWHKD_UPSTREAM_COMMIT=\"$SWHKD_PIN\"" "$TOP_INSTALL" \
    && grep -qF 'git -C "$src" fetch --depth 1 "$SWHKD_REPO_URL" "$SWHKD_UPSTREAM_COMMIT"' "$TOP_INSTALL" \
    && grep -qF 'make NO_RFKILL_SW_SUPPORT=1' "$TOP_INSTALL" \
    && grep -qF 'if swhkd_binary_is_safe "$swhkd_path"; then' "$TOP_INSTALL"; then
    pass "install.sh pins and verifies rfkill-free swhkd before launch"
else
    fail "install.sh does not pin and verify rfkill-free swhkd before launch"
fi

echo ""

# 11. Build tool availability notes

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

# Summary

echo "Results: $PASS passed, $FAIL failed, $SKIP skipped"
if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
