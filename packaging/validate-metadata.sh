#!/usr/bin/env bash






set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

PASS=0
FAIL=0

pass() { printf '[PASS] %s\n' "$1"; PASS=$((PASS + 1)); }
fail() { printf '[FAIL] %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }
note() { printf '[NOTE] %s\n' "$1"; }



CARGO_TOML="$REPO_ROOT/src/Cargo.toml"
EXPECTED_APP_ID="com.linuxsoundboard.app"
EXPECTED_BINARY="linux-soundboard"
EXPECTED_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$CARGO_TOML" | head -n 1)"
PUBLIC_VERSION="$(sed -n 's/^## \[\([0-9][0-9.]*\)\].*/\1/p' "$REPO_ROOT/docs/CHANGELOG.md" | head -n 1)"

if [[ -z "$EXPECTED_VERSION" ]]; then
    echo "ERROR: could not read version from $CARGO_TOML" >&2
    exit 1
fi
if [[ -z "$PUBLIC_VERSION" ]]; then
    echo "ERROR: could not read latest public version from docs/CHANGELOG.md" >&2
    exit 1
fi

note "Development version from Cargo.toml: $EXPECTED_VERSION"
note "Latest public version from changelog: $PUBLIC_VERSION"
note "App ID:    $EXPECTED_APP_ID"
note "Binary:    $EXPECTED_BINARY"
echo ""



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
    if [[ "$found" == "$PUBLIC_VERSION" ]]; then
        pass "$label: version $PUBLIC_VERSION"
    else
        fail "$label: expected latest public version $PUBLIC_VERSION, got '$found' in $file"
    fi
}

note "AUR/DEB/RPM version checks skipped: current release policy is AppImage-only"

check_version_in_file "metainfo.xml latest release" \
    "$REPO_ROOT/packaging/flatpak/com.linuxsoundboard.app.metainfo.xml" \
    '(?<=<release version=")[\d.]+'

echo ""



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
if grep -qF "source \"\$SCRIPT_DIR/app-meta.sh\"" "$REPO_ROOT/packaging/linux/install-user.sh" && \
   grep -qF "\$APP_BINARY" "$REPO_ROOT/packaging/linux/install-user.sh"; then
    pass "install-user.sh: binary metadata sourced from app-meta.sh"
else
    fail "install-user.sh: expected app-meta.sh source and APP_BINARY usage"
fi
check_binary_in_file "app-meta.sh"                "$REPO_ROOT/packaging/linux/app-meta.sh"
check_binary_in_file "engine service"             "$REPO_ROOT/packaging/linux/linux-soundboard-engine.service"

echo ""



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



SERVICE="$REPO_ROOT/packaging/linux/linux-soundboard-engine.service"
TARGET="$REPO_ROOT/packaging/linux/linux-soundboard-engine.target"
if [[ ! -f "$SERVICE" ]]; then
    fail "engine service: file not found"
else
    for field in "Description=" "ExecStart=" "Type=" "Restart=" "PartOf=linux-soundboard-engine.target" "RefuseManualStop=yes"; do
        if grep -qF "$field" "$SERVICE"; then
            pass "engine service: $field present"
        else
            fail "engine service: missing $field"
        fi
    done
fi
if [[ ! -f "$TARGET" ]]; then
    fail "engine target: file not found"
else
    for field in "Wants=linux-soundboard-engine.service" "WantedBy=default.target"; do
        if grep -qF "$field" "$TARGET"; then
            pass "engine target: $field present"
        else
            fail "engine target: missing $field"
        fi
    done
fi

echo ""



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



if [[ -f "$REPO_ROOT/packaging/deb/control" ]]; then
    note "packaging/deb/control exists — this is a legacy artifact predating packaging/debian/."
    note "It is not used by any build script. Review and remove when convenient."
fi

echo ""



echo "Results: $PASS passed, $FAIL failed"
if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
