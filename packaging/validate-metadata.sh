#!/usr/bin/env bash
#
# Cross-validates packaging metadata consistency for Linux Soundboard.
# Run from the repository root or any subdirectory.
#
# Exit codes: 0 = all checks pass, 1 = one or more failures.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

PASS=0
FAIL=0

pass() { printf '[PASS] %s\n' "$1"; PASS=$((PASS + 1)); }
fail() { printf '[FAIL] %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }
note() { printf '[NOTE] %s\n' "$1"; }

# ── Source of truth ──────────────────────────────────────────────────────────

CARGO_TOML="$REPO_ROOT/src/Cargo.toml"
EXPECTED_APP_ID="com.linuxsoundboard.app"
EXPECTED_BINARY="linux-soundboard"
EXPECTED_APP_NAME="Linux Soundboard"
EXPECTED_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$CARGO_TOML" | head -n 1)"

if [[ -z "$EXPECTED_VERSION" ]]; then
    echo "ERROR: could not read version from $CARGO_TOML" >&2
    exit 1
fi

note "Version from Cargo.toml: $EXPECTED_VERSION"
note "App ID:    $EXPECTED_APP_ID"
note "Binary:    $EXPECTED_BINARY"
echo ""

# ── Version consistency ──────────────────────────────────────────────────────

check_version_in_file() {
    local label="$1"
    local file="$2"
    local pattern="$3"

    if [[ ! -f "$file" ]]; then
        fail "$label: file not found: $file"
        return
    fi
    local found
    found="$(grep -oP "$pattern" "$file" | head -n 1 || true)"
    if [[ "$found" == "$EXPECTED_VERSION" ]]; then
        pass "$label: version $EXPECTED_VERSION"
    else
        fail "$label: expected version $EXPECTED_VERSION, got '$found' in $file"
    fi
}

check_version_in_file "RPM spec" \
    "$REPO_ROOT/packaging/rpm/linux-soundboard.spec" \
    '(?<=^Version:\s{8})[\d.]+'

check_version_in_file "AUR stable PKGBUILD" \
    "$REPO_ROOT/packaging/aur/PKGBUILD" \
    '(?<=^pkgver=)[\d.]+'

check_version_in_file "AUR stable .SRCINFO" \
    "$REPO_ROOT/packaging/aur/.SRCINFO" \
    '(?<=pkgver = )[\d.]+'

check_version_in_file "debian/changelog" \
    "$REPO_ROOT/packaging/debian/changelog" \
    "(?<=linux-soundboard \()[\d.]+"

check_version_in_file "metainfo.xml latest release" \
    "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.metainfo.xml" \
    '(?<=<release version=")[\d.]+'

echo ""

# ── App ID consistency ───────────────────────────────────────────────────────

check_app_id_in_file() {
    local label="$1"
    local file="$2"

    if [[ ! -f "$file" ]]; then
        fail "$label: file not found: $file"
        return
    fi
    if grep -qF "$EXPECTED_APP_ID" "$file"; then
        pass "$label: app-id present"
    else
        fail "$label: app-id '$EXPECTED_APP_ID' not found in $file"
    fi
}

check_app_id_in_file "Flatpak manifest"          "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.yml"
check_app_id_in_file "Flatpak desktop file"      "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.desktop"
check_app_id_in_file "Flatpak metainfo"          "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.metainfo.xml"
check_app_id_in_file "Debian desktop file"       "$REPO_ROOT/packaging/debian/linux-soundboard.desktop"
check_app_id_in_file "RPM desktop file"          "$REPO_ROOT/packaging/rpm/linux-soundboard.desktop"
check_app_id_in_file "RPM spec %files"           "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"
check_app_id_in_file "AUR stable PKGBUILD"       "$REPO_ROOT/packaging/aur/PKGBUILD"
check_app_id_in_file "AUR git PKGBUILD"          "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD"

echo ""

# ── Binary name consistency ──────────────────────────────────────────────────

check_binary_in_file() {
    local label="$1"
    local file="$2"

    if [[ ! -f "$file" ]]; then
        fail "$label: file not found: $file"
        return
    fi
    if grep -qF "$EXPECTED_BINARY" "$file"; then
        pass "$label: binary '$EXPECTED_BINARY' present"
    else
        fail "$label: binary '$EXPECTED_BINARY' not found in $file"
    fi
}

check_binary_in_file "Flatpak manifest (command)" "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.yml"
check_binary_in_file "RPM spec (%files)"          "$REPO_ROOT/packaging/rpm/linux-soundboard.spec"
check_binary_in_file "AUR stable PKGBUILD"        "$REPO_ROOT/packaging/aur/PKGBUILD"
check_binary_in_file "AUR git PKGBUILD"           "$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD"
check_binary_in_file "install-user.sh"            "$REPO_ROOT/packaging/linux/install-user.sh"
check_binary_in_file "app-meta.sh"                "$REPO_ROOT/packaging/linux/app-meta.sh"
check_binary_in_file "engine service"             "$REPO_ROOT/packaging/linux/linux-soundboard-engine.service"

echo ""

# ── Desktop file fields ──────────────────────────────────────────────────────

check_desktop_fields() {
    local label="$1"
    local file="$2"

    if [[ ! -f "$file" ]]; then
        fail "$label: file not found: $file"
        return
    fi
    local ok=1
    for field in "Name=" "Exec=" "Icon=" "Type=Application"; do
        if ! grep -qF "$field" "$file"; then
            fail "$label: missing field '$field'"
            ok=0
        fi
    done
    if [[ "$ok" -eq 1 ]]; then
        pass "$label: required fields present"
    fi
}

check_desktop_fields "Flatpak desktop" "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.desktop"
check_desktop_fields "Debian desktop"  "$REPO_ROOT/packaging/debian/linux-soundboard.desktop"
check_desktop_fields "RPM desktop"     "$REPO_ROOT/packaging/rpm/linux-soundboard.desktop"

echo ""

# ── Metainfo required fields ─────────────────────────────────────────────────

METAINFO="$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.metainfo.xml"
if [[ ! -f "$METAINFO" ]]; then
    fail "metainfo.xml: file not found"
else
    for tag in "<id>" "<name>" "<summary>" "<description>" "<releases>" "<metadata_license>" "<project_license>"; do
        if grep -qF "$tag" "$METAINFO"; then
            pass "metainfo.xml: $tag present"
        else
            fail "metainfo.xml: missing $tag"
        fi
    done
fi

echo ""

# ── Service file fields ──────────────────────────────────────────────────────

SERVICE="$REPO_ROOT/packaging/linux/linux-soundboard-engine.service"
if [[ ! -f "$SERVICE" ]]; then
    fail "engine service: file not found"
else
    for field in "Description=" "ExecStart=" "Type=" "Restart=" "WantedBy="; do
        if grep -qF "$field" "$SERVICE"; then
            pass "engine service: $field present"
        else
            fail "engine service: missing $field"
        fi
    done
fi

echo ""

# ── Icon files ───────────────────────────────────────────────────────────────

ICON_ROOT="$REPO_ROOT/src/resources/icons"
ICON_SIZES=(16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512)
ICON_NAMES=(com.linuxsoundboard.app.png linux-soundboard.png)

for size in "${ICON_SIZES[@]}"; do
    for name in "${ICON_NAMES[@]}"; do
        path="$ICON_ROOT/$size/apps/$name"
        if [[ -f "$path" ]]; then
            pass "icon: $size/apps/$name"
        else
            fail "icon: missing $path"
        fi
    done
done

echo ""

# ── Metainfo installed by all native package formats ────────────────────────

for label_and_file in \
    "RPM spec:$REPO_ROOT/packaging/rpm/linux-soundboard.spec" \
    "Debian rules:$REPO_ROOT/packaging/debian/rules" \
    "AUR stable PKGBUILD:$REPO_ROOT/packaging/aur/PKGBUILD" \
    "AUR git PKGBUILD:$REPO_ROOT/packaging/aur/linux-soundboard-git/PKGBUILD"
do
    label="${label_and_file%%:*}"
    file="${label_and_file#*:}"
    if grep -qF "metainfo" "$file"; then
        pass "$label: installs metainfo"
    else
        fail "$label: does not install metainfo"
    fi
done

echo ""

# ── Known stale artifacts ────────────────────────────────────────────────────

if [[ -f "$REPO_ROOT/packaging/deb/control" ]]; then
    note "packaging/deb/control exists — this is a legacy artifact predating packaging/debian/."
    note "It is not used by any build script. Review and remove when convenient."
fi

echo ""

# ── Summary ──────────────────────────────────────────────────────────────────

echo "Results: $PASS passed, $FAIL failed"
if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
