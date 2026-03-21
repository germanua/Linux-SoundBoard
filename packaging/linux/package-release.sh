#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/app-meta.sh"

REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
MANIFEST_PATH="$REPO_ROOT/src-tauri/Cargo.toml"
ICON_SOURCE="$REPO_ROOT/icon.png"
ICON_SOURCE_ROOT="$REPO_ROOT/src-tauri/resources/icons"
BINARY_SOURCE="$REPO_ROOT/src-tauri/target/release/$APP_BINARY"
DIST_ROOT="$REPO_ROOT/dist"

version="$(
    sed -n 's/^version = "\(.*\)"$/\1/p' "$MANIFEST_PATH" | head -n 1
)"
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
Icon=$APP_ID
Terminal=false
Categories=AudioVideo;Audio;
Keywords=soundboard;audio;pipewire;microphone;
StartupNotify=true
StartupWMClass=$APP_BINARY
EOF

while IFS= read -r icon_path; do
    size_dir="$(basename "$(dirname "$(dirname "$icon_path")")")"
    install -Dm644 "$icon_path" "$bundle_dir/icons/$size_dir/apps/$APP_ID.png"
done < <(find "$ICON_SOURCE_ROOT" -path "*/apps/$APP_ID.png" -type f | sort)

tar -C "$DIST_ROOT" -czf "$archive_path" "$bundle_name"

echo "Created release bundle:"
echo "  Directory: $bundle_dir"
echo "  Archive:   $archive_path"
echo
echo "Users can extract the archive and run ./install-user.sh to install the desktop launcher and icon."
