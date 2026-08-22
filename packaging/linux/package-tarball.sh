#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/app-meta.sh"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/../common.sh"

REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
MANIFEST_PATH="$REPO_ROOT/src/Cargo.toml"
ICON_SOURCE="$REPO_ROOT/assets/icons/icon.png"
ICON_SOURCE_ROOT="$REPO_ROOT/src/resources/icons"
BINARY_SOURCE="$REPO_ROOT/target/release/$APP_BINARY"
SWHKD_HELPER_SOURCE="$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"
INSTALLER_SOURCE="$REPO_ROOT/packaging/linux/install-user.sh"
APP_META_SOURCE="$REPO_ROOT/packaging/linux/app-meta.sh"
DIST_ROOT="$REPO_ROOT/dist"

version="$(cargo_version_from_manifest "$MANIFEST_PATH")" || exit 1
arch="$(uname -m)"

bundle_name="${APP_BINARY}-${version}-linux-${arch}"
bundle_dir="$DIST_ROOT/$bundle_name"
tarball_path="$DIST_ROOT/${bundle_name}.tar.gz"

build_project=1
for arg in "$@"; do
    case "$arg" in
        --skip-build)
            build_project=0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            echo "Usage: $0 [--skip-build]" >&2
            exit 1
            ;;
    esac
done

if [[ "$build_project" -eq 1 ]]; then
    "$SCRIPT_DIR/generate-icons.sh" "$ICON_SOURCE"
    cargo build --locked --release --manifest-path "$MANIFEST_PATH"
fi

if [[ ! -x "$BINARY_SOURCE" ]]; then
    echo "Expected built binary at $BINARY_SOURCE" >&2
    exit 1
fi

rm -rf "$bundle_dir"
mkdir -p "$DIST_ROOT" "$bundle_dir"

install -Dm755 "$BINARY_SOURCE" "$bundle_dir/$APP_BINARY"
install -Dm755 "$INSTALLER_SOURCE" "$bundle_dir/install-user.sh"
install -Dm644 "$APP_META_SOURCE" "$bundle_dir/app-meta.sh"
install -Dm755 "$SWHKD_HELPER_SOURCE" "$bundle_dir/install-swhkd-helper.sh"

for legal_file in LICENSE NOTICE.md THIRDPARTY_LICENSES.md THIRD_PARTY_NOTICES.html COMMERCIAL-LICENSE.md DONATIONS.md README.md; do
    install -Dm644 "$REPO_ROOT/$legal_file" "$bundle_dir/$legal_file"
done

while IFS= read -r icon_path; do
    relative_path="${icon_path#"$ICON_SOURCE_ROOT/"}"
    install -Dm644 "$icon_path" "$bundle_dir/icons/$relative_path"
done < <(find "$ICON_SOURCE_ROOT" -type f | sort)

rm -f "$tarball_path"
tar -czf "$tarball_path" -C "$DIST_ROOT" "$bundle_name"
rm -rf "$bundle_dir"

# Refreshed over everything in dist/, so the list covers the .deb, .rpm, and
# AppImage built before it as well.
"$REPO_ROOT/packaging/generate-checksums.sh" "$DIST_ROOT" >/dev/null

echo "Created tarball artifact:"
echo "  $tarball_path"
