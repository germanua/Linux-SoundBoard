#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/app-meta.sh"

REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
MANIFEST_PATH="$REPO_ROOT/src/Cargo.toml"
ICON_SOURCE="$REPO_ROOT/assets/icons/icon.png"
ICON_SOURCE_ROOT="$REPO_ROOT/src/resources/icons"
BINARY_SOURCE="$REPO_ROOT/src/target/release/$APP_BINARY"
SWHKD_HELPER_SOURCE="$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"
DIST_ROOT="$REPO_ROOT/dist"
TOOLS_ROOT="$DIST_ROOT/.appimage-tools"

TAURI_GTK_PLUGIN_COMMIT="f0381b4bdf607bbf5fc5dfe3a60a64609a26ff23"

version="$(
    sed -n 's/^version = "\(.*\)"$/\1/p' "$MANIFEST_PATH" | head -n 1
)"

arch="$(uname -m)"
case "$arch" in
    x86_64)
        linuxdeploy_arch="x86_64"
        ;;
    aarch64|arm64)
        linuxdeploy_arch="aarch64"
        ;;
    *)
        echo "Unsupported architecture for AppImage packaging: $arch" >&2
        exit 1
        ;;
esac

LINUXDEPLOY_URL="${LINUXDEPLOY_URL:-https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${linuxdeploy_arch}.AppImage}"
GTK_PLUGIN_URL="${GTK_PLUGIN_URL:-https://raw.githubusercontent.com/tauri-apps/tauri/${TAURI_GTK_PLUGIN_COMMIT}/crates/tauri-bundler/src/bundle/linux/appimage/linuxdeploy-plugin-gtk.sh}"

LINUXDEPLOY_APPIMAGE="$TOOLS_ROOT/linuxdeploy-${linuxdeploy_arch}.AppImage"
LINUXDEPLOY_ROOT="$TOOLS_ROOT/linuxdeploy-${linuxdeploy_arch}.root"
LINUXDEPLOY_BIN="$LINUXDEPLOY_ROOT/usr/bin/linuxdeploy"
GTK_PLUGIN_BIN="$TOOLS_ROOT/linuxdeploy-plugin-gtk.sh"

APPDIR="$DIST_ROOT/${APP_BINARY}.AppDir"
DESKTOP_FILE="$DIST_ROOT/$APP_ID.desktop"
METAINFO_FILE="$DIST_ROOT/$APP_ID.metainfo.xml"

versioned_name="${APP_BINARY}-${version}-${linuxdeploy_arch}.AppImage"
stable_name="${APP_BINARY}-${linuxdeploy_arch}.AppImage"
versioned_path="$DIST_ROOT/$versioned_name"
stable_path="$DIST_ROOT/$stable_name"

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

download_if_missing() {
    local url="$1"
    local output="$2"

    if [[ -f "$output" ]]; then
        return 0
    fi

    local tmp="${output}.tmp"
    curl -fsSL "$url" -o "$tmp"
    mv "$tmp" "$output"
}

extract_linuxdeploy() {
    if [[ -x "$LINUXDEPLOY_BIN" ]]; then
        return 0
    fi

    local extract_dir="$TOOLS_ROOT/.linuxdeploy-extract"
    rm -rf "$extract_dir" "$LINUXDEPLOY_ROOT"
    mkdir -p "$extract_dir"
    (
        cd "$extract_dir"
        "$LINUXDEPLOY_APPIMAGE" --appimage-extract >/dev/null
    )
    mv "$extract_dir/squashfs-root" "$LINUXDEPLOY_ROOT"
    rm -rf "$extract_dir"

    # The bundled strip is too old for some modern RELR-enabled host libraries.
    rm -f "$LINUXDEPLOY_ROOT/usr/bin/strip"
    ln -s /usr/bin/strip "$LINUXDEPLOY_ROOT/usr/bin/strip"
}

mkdir -p "$DIST_ROOT" "$TOOLS_ROOT"

if [[ "$build_project" -eq 1 ]]; then
    "$SCRIPT_DIR/generate-icons.sh" "$ICON_SOURCE"
    cargo build --release --manifest-path "$MANIFEST_PATH"
fi

if [[ ! -x "$BINARY_SOURCE" ]]; then
    echo "Expected built binary at $BINARY_SOURCE" >&2
    exit 1
fi

download_if_missing "$LINUXDEPLOY_URL" "$LINUXDEPLOY_APPIMAGE"
download_if_missing "$GTK_PLUGIN_URL" "$GTK_PLUGIN_BIN"

chmod +x "$LINUXDEPLOY_APPIMAGE" "$GTK_PLUGIN_BIN"
extract_linuxdeploy

if ! grep -q 'DEPLOY_GTK_VERSION="${DEPLOY_GTK_VERSION:-4}"' "$GTK_PLUGIN_BIN"; then
    sed -i 's/^DEPLOY_GTK_VERSION=3.*/DEPLOY_GTK_VERSION="${DEPLOY_GTK_VERSION:-4}" # Patched for Linux Soundboard GTK4 packaging/' "$GTK_PLUGIN_BIN"
fi

if grep -q 'find /usr/lib\* -name libgiognutls.so' "$GTK_PLUGIN_BIN"; then
    sed -i 's|find /usr/lib\\* -name libgiognutls.so|find /usr/lib -name libgiognutls.so|' "$GTK_PLUGIN_BIN"
fi

# Skip VMware shim libraries; they drag host-only dependencies into the AppImage.
if grep -Fq 'done < <(find "$directory" \( -type l -o -type f \) -name "$library" -print0)' "$GTK_PLUGIN_BIN"; then
    sed -i '/done < <(find "\$directory" \\( -type l -o -type f \\) -name "\$library" -print0)/c\    done < <(find "$directory" \( -type l -o -type f \) ! -path "/usr/lib/vmware/*" ! -path "/usr/lib/vmware/**" -name "$library" -print0)' "$GTK_PLUGIN_BIN"
fi

# Patch GTK plugin for Wayland support.
echo "Patching GTK plugin for Wayland support..."
if grep -q 'export GDK_BACKEND=x11' "$GTK_PLUGIN_BIN"; then
    sed -i 's/export GDK_BACKEND=x11.*/# Prefer Wayland when available; fall back to X11 if needed/' "$GTK_PLUGIN_BIN"
    # Insert backend detection after the marker.
    sed -i '/# Prefer Wayland when available; fall back to X11 if needed/a \
\
# Wayland first, X11 as fallback.\
if [ -z "$GDK_BACKEND" ]; then\
    if [ -n "$WAYLAND_DISPLAY" ] \&\& [ -z "$LSB_FORCE_X11" ]; then\
        export GDK_BACKEND=wayland\
    elif [ -n "$DISPLAY" ]; then\
        export GDK_BACKEND=x11\
    fi\
fi' "$GTK_PLUGIN_BIN"
    echo "✓ Wayland support enabled in GTK plugin"
fi

rm -rf "$APPDIR"
rm -f "$versioned_path" "$stable_path"
mkdir -p \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/metainfo" \
    "$APPDIR/usr/libexec/linux-soundboard"

install -Dm755 "$SWHKD_HELPER_SOURCE" "$APPDIR/usr/libexec/linux-soundboard/install-swhkd-helper.sh"
install -Dm755 "$SWHKD_HELPER_SOURCE" "$APPDIR/usr/bin/install-swhkd-helper.sh"

cat >"$DESKTOP_FILE" <<EOF
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

cat >"$METAINFO_FILE" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>$APP_ID</id>
  <name>$APP_NAME</name>
  <summary>$APP_COMMENT</summary>
  <metadata_license>CC0-1.0</metadata_license>
  <project_license>PolyForm-Noncommercial-1.0.0</project_license>
  <launchable type="desktop-id">$APP_ID.desktop</launchable>
  <description>
    <p>$APP_COMMENT</p>
  </description>
  <categories>
    <category>AudioVideo</category>
    <category>Audio</category>
  </categories>
</component>
EOF

install -Dm644 "$DESKTOP_FILE" "$APPDIR/usr/share/applications/$APP_ID.desktop"
install -Dm644 "$METAINFO_FILE" "$APPDIR/usr/share/metainfo/$APP_ID.metainfo.xml"

while IFS= read -r icon_path; do
    size_dir="$(basename "$(dirname "$(dirname "$icon_path")")")"
    for icon_name in "$APP_ID" "$APP_ICON_NAME"; do
        install -Dm644 "$icon_path" "$APPDIR/usr/share/icons/hicolor/$size_dir/apps/$icon_name.png"
    done
done < <(find "$ICON_SOURCE_ROOT" -path "*/apps/$APP_ID.png" -type f | sort)

(
    cd "$DIST_ROOT"
    export DEPLOY_GTK_VERSION=4
    export PATH="$TOOLS_ROOT:$LINUXDEPLOY_ROOT/usr/bin:$PATH"
    "$LINUXDEPLOY_BIN" \
        --appdir "$APPDIR" \
        --executable "$BINARY_SOURCE" \
        --desktop-file "$APPDIR/usr/share/applications/$APP_ID.desktop" \
        --icon-file "$ICON_SOURCE_ROOT/512x512/apps/$APP_ICON_NAME.png" \
        --plugin gtk
)

rm -rf "$APPDIR/usr/lib32"

# The AppImage uses the host PipeWire and WirePlumber stack.
echo "Skipping pactl bundling; the runtime now uses native PipeWire streams and host wpctl."

# Trim unused libraries to keep the AppImage small.
echo "Removing unnecessary libraries..."

# Drop codecs and image loaders the app does not use.
rm -f "$APPDIR/usr/lib"/libopenraw* 2>/dev/null || true      # RAW image support
rm -f "$APPDIR/usr/lib"/libglycin* 2>/dev/null || true       # Image loader
rm -f "$APPDIR/usr/lib"/libdav1d* 2>/dev/null || true        # AV1 video codec
rm -f "$APPDIR/usr/lib"/libavif* 2>/dev/null || true         # AVIF images
rm -f "$APPDIR/usr/lib"/libheif* 2>/dev/null || true         # HEIF images
rm -f "$APPDIR/usr/lib"/libjxl* 2>/dev/null || true          # JPEG XL
rm -f "$APPDIR/usr/lib"/libde265* 2>/dev/null || true        # HEVC decoder
rm -f "$APPDIR/usr/lib"/libx265* 2>/dev/null || true         # HEVC encoder
rm -f "$APPDIR/usr/lib"/libkvazaar* 2>/dev/null || true      # HEVC encoder
rm -f "$APPDIR/usr/lib"/libSvtAv1* 2>/dev/null || true       # AV1 encoder
rm -f "$APPDIR/usr/lib"/libaom* 2>/dev/null || true          # AV1 codec
rm -f "$APPDIR/usr/lib"/librav1e* 2>/dev/null || true        # AV1 encoder

echo "Library cleanup complete"

# Add startup dependency checks.
echo "Adding preflight dependency checker..."
install -Dm755 "$SCRIPT_DIR/appimage-preflight-check.sh" "$APPDIR/usr/bin/appimage-preflight-check"

# Wire the preflight check into AppRun.
if [ -f "$APPDIR/AppRun" ]; then
    # Insert the check before the final exec line.
    sed -i '/^exec "$this_dir"\/AppRun.wrapped/i \
# Run preflight checks (skip with SKIP_PREFLIGHT_CHECK=1).\
if [ -z "$SKIP_PREFLIGHT_CHECK" ]; then\
    "$this_dir"/usr/bin/appimage-preflight-check || exit 1\
fi\
' "$APPDIR/AppRun"
    echo "✓ Preflight checker integrated into AppRun"
fi

(
    cd "$DIST_ROOT"
    export PATH="$TOOLS_ROOT:$LINUXDEPLOY_ROOT/usr/bin:$PATH"
    export ARCH="$linuxdeploy_arch"
    export LDAI_OUTPUT="$versioned_name"
    "$LINUXDEPLOY_BIN" \
        --appdir "$APPDIR" \
        --output appimage
)

cp "$versioned_path" "$stable_path"

echo "Created AppImage artifacts:"
echo "  Versioned: $versioned_path"
echo "  Stable:    $stable_path"
