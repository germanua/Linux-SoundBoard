#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/app-meta.sh"
source "$SCRIPT_DIR/../common.sh"

REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
MANIFEST_PATH="$REPO_ROOT/src/Cargo.toml"
ICON_SOURCE="$REPO_ROOT/assets/icons/icon.png"
ICON_SOURCE_ROOT="$REPO_ROOT/src/resources/icons"
BINARY_SOURCE="$REPO_ROOT/src/target/release/$APP_BINARY"
DIST_ROOT="$REPO_ROOT/dist"
SWHKD_HELPER_SOURCE="$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"

version="$(cargo_version_from_manifest "$MANIFEST_PATH")" || exit 1
arch="$(uname -m)"
bundle_name="${APP_BINARY}-${version}-linux-${arch}"
bundle_dir="$DIST_ROOT/$bundle_name"
archive_path="$DIST_ROOT/$bundle_name.tar.gz"

if [[ "${1:-}" != "--skip-build" ]]; then
    "$SCRIPT_DIR/generate-icons.sh" "$ICON_SOURCE"
    cargo build --release --manifest-path "$MANIFEST_PATH"
fi

if [[ ! -x "$BINARY_SOURCE" ]]; then
    echo "Expected built binary at $BINARY_SOURCE" >&2
    exit 1
fi

rm -rf "$bundle_dir"
mkdir -p "$bundle_dir/icons"

install -Dm755 "$BINARY_SOURCE" "$bundle_dir/$APP_BINARY"
install -Dm755 "$SWHKD_HELPER_SOURCE" "$bundle_dir/install-swhkd-helper.sh"
install -Dm755 "$SCRIPT_DIR/install-user.sh" "$bundle_dir/install-user.sh"
install -Dm755 "$SCRIPT_DIR/app-meta.sh" "$bundle_dir/app-meta.sh"
install -Dm644 "$REPO_ROOT/README.md" "$bundle_dir/README.md"

cat >"$bundle_dir/$APP_ID.desktop" <<EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=$APP_NAME
Comment=$APP_COMMENT
Exec=$APP_BINARY
Icon=$APP_ICON_NAME
Terminal=false
Categories=AudioVideo;Audio;
Keywords=soundboard;audio;pipewire;microphone;
StartupNotify=true
StartupWMClass=$APP_BINARY
EOF

while IFS= read -r icon_path; do
    size_dir="$(basename "$(dirname "$(dirname "$icon_path")")")"
    for icon_name in "$APP_ID" "$APP_ICON_NAME"; do
        install -Dm644 "$icon_path" "$bundle_dir/icons/$size_dir/apps/$icon_name.png"
    done
done < <(find "$ICON_SOURCE_ROOT" -path "*/apps/$APP_ID.png" -type f | sort)

tar -C "$DIST_ROOT" -czf "$archive_path" "$bundle_name"

"$SCRIPT_DIR/package-appimage.sh" --skip-build

cat >"$DIST_ROOT/release-notes.md" <<EOF
## Linux Soundboard $version

- Full native Wayland and X11 support
- Wayland global hotkeys via swhkd
- Native X11/XWayland hotkey backend
- AppImage, DEB, RPM, Flatpak, and AUR packaging support

### Included in this local release bundle

- $bundle_name.tar.gz
- ${APP_BINARY}-$(uname -m).AppImage

### Distribution notes

- Ubuntu/Debian: build or ship the DEB package from \`packaging/debian/\`
- Fedora/RHEL: build or ship the RPM package from \`packaging/rpm/\`
- Flatpak: build or ship the bundle from \`packaging/flatpak/\`
- Arch Linux: update both \`linux-soundboard\` and \`linux-soundboard-git\` AUR files
EOF

echo "Created release bundle:"
echo "  Directory: $bundle_dir"
echo "  Archive:   $archive_path"
echo "  AppImage:  $DIST_ROOT/${APP_BINARY}-$(uname -m).AppImage"
echo "  Notes:     $DIST_ROOT/release-notes.md"
echo
echo "Users can extract the archive and run ./install-user.sh to install the desktop launcher and icon."
