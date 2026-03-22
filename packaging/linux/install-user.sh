#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/app-meta.sh"

INSTALL_ROOT="${INSTALL_ROOT:-$HOME/.local/opt/linux-soundboard}"
INSTALL_BINARY="$INSTALL_ROOT/$APP_BINARY"
XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
DESKTOP_DIR="$XDG_DATA_HOME/applications"
ICON_THEME_DIR="$XDG_DATA_HOME/icons/hicolor"

if [[ $# -ge 1 ]]; then
    BINARY_SOURCE="$(realpath "$1")"
elif [[ -x "$SCRIPT_DIR/$APP_BINARY" ]]; then
    BINARY_SOURCE="$SCRIPT_DIR/$APP_BINARY"
elif [[ -x "$SCRIPT_DIR/../../src/target/release/$APP_BINARY" ]]; then
    BINARY_SOURCE="$(realpath "$SCRIPT_DIR/../../src/target/release/$APP_BINARY")"
else
    echo "Could not find a built $APP_BINARY binary." >&2
    echo "Pass the path to the release binary as the first argument, or run this after packaging." >&2
    exit 1
fi

if [[ -d "$SCRIPT_DIR/icons" ]]; then
    ICON_SOURCE_ROOT="$SCRIPT_DIR/icons"
elif [[ -d "$SCRIPT_DIR/../../src/resources/icons" ]]; then
    ICON_SOURCE_ROOT="$(realpath "$SCRIPT_DIR/../../src/resources/icons")"
else
    echo "Could not find the bundled icon set." >&2
    exit 1
fi

install -Dm755 "$BINARY_SOURCE" "$INSTALL_BINARY"

while IFS= read -r icon_path; do
    size_dir="$(basename "$(dirname "$(dirname "$icon_path")")")"
    install -Dm644 "$icon_path" "$ICON_THEME_DIR/$size_dir/apps/$APP_ID.png"
done < <(find "$ICON_SOURCE_ROOT" -path "*/apps/$APP_ID.png" -type f | sort)

desktop_file="$DESKTOP_DIR/$APP_ID.desktop"
mkdir -p "$DESKTOP_DIR"
cat >"$desktop_file" <<EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=$APP_NAME
Comment=$APP_COMMENT
Exec=$INSTALL_BINARY
Icon=$APP_ID
Terminal=false
Categories=AudioVideo;Audio;
Keywords=soundboard;audio;pipewire;microphone;
StartupNotify=true
StartupWMClass=$APP_BINARY
EOF

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$ICON_THEME_DIR" >/dev/null 2>&1 || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$DESKTOP_DIR" >/dev/null 2>&1 || true
fi

echo "Installed $APP_NAME:"
echo "  Binary:   $INSTALL_BINARY"
echo "  Launcher: $desktop_file"
echo
echo "Your desktop environment may need a few seconds to refresh the app list."
