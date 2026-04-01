#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
source "$SCRIPT_DIR/app-meta.sh"

SOURCE_ICON="${1:-$REPO_ROOT/assets/icons/icon.png}"
ICON_ROOT="$REPO_ROOT/src/resources/icons"
SIZES=(16 24 32 48 64 128 256 512)

if [[ ! -f "$SOURCE_ICON" ]]; then
    echo "Source icon not found: $SOURCE_ICON" >&2
    exit 1
fi

if ! command -v magick >/dev/null 2>&1; then
    echo "ImageMagick 'magick' command is required." >&2
    exit 1
fi

for size in "${SIZES[@]}"; do
    target_dir="$ICON_ROOT/${size}x${size}/apps"
    primary_target="$target_dir/$APP_ID.png"
    alias_target="$target_dir/$APP_ICON_NAME.png"
    mkdir -p "$target_dir"

    magick "$SOURCE_ICON" \
        -background none \
        -gravity center \
        -resize "${size}x${size}" \
        -extent "${size}x${size}" \
        "$primary_target"

    if [[ "$alias_target" != "$primary_target" ]]; then
        cp "$primary_target" "$alias_target"
    fi
done

echo "Updated app icons from: $SOURCE_ICON"
