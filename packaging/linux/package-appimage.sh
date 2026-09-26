#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"


source "$SCRIPT_DIR/app-meta.sh"


source "$SCRIPT_DIR/../common.sh"

REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
MANIFEST_PATH="$REPO_ROOT/src/Cargo.toml"
ICON_SOURCE_ROOT="$REPO_ROOT/src/resources/icons"
CARGO_BINARY_SOURCE="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/release/linux-soundboard"
BINARY_SOURCE="$CARGO_BINARY_SOURCE"
SWHKD_HELPER_SOURCE="$REPO_ROOT/packaging/linux/install-swhkd-helper.sh"
SWHKD_BUILD_SCRIPT_SOURCE="$REPO_ROOT/packaging/linux/build-swhkd-locked.sh"
SWHKD_PINNED_LOCK_SOURCE="$REPO_ROOT/packaging/linux/swhkd-Cargo.lock.pinned"
INSTALLER_SOURCE="$REPO_ROOT/packaging/linux/install-user.sh"
APP_META_RENDERED=""
DIST_ROOT="${LSB_DIST_ROOT:-$REPO_ROOT/dist}"
TOOLS_ROOT="$DIST_ROOT/.appimage-tools"

TAURI_GTK_PLUGIN_COMMIT="f0381b4bdf607bbf5fc5dfe3a60a64609a26ff23"

if [[ "$LSB_BUILD_PROFILE" == "dev" ]]; then
    version="${LSB_DEV_VERSION:-}"
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-dev\.[0-9]+$ ]] \
        || { echo "DEV AppImage build requires LSB_DEV_VERSION=X.Y.Z-dev.N" >&2; exit 1; }
else
    version="$(cargo_version_from_manifest "$MANIFEST_PATH")" || exit 1
fi

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

LINUXDEPLOY_RELEASE="1-alpha-20251107-1"
APPIMAGE_RUNTIME_RELEASE="20251108"
APPIMAGE_RUNTIME_GPG_KEY="570C 77AC EA40 C0F1 B758 902C BF96 CCA5 6490 F695"
LINUXDEPLOY_PLUGIN_RELEASE="1-alpha-20250213-1"
GTK_PLUGIN_SHA256="06a56df39e65806170ebae570e593ea14ad9aecf97f668694c343f461482b4c4"

case "$linuxdeploy_arch" in
    x86_64)
        LINUXDEPLOY_SHA256="c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"
        APPIMAGE_RUNTIME_SHA256="2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"
        LINUXDEPLOY_PLUGIN_SHA256="992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3"
        ;;
    aarch64)
        LINUXDEPLOY_SHA256="620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff"
        APPIMAGE_RUNTIME_SHA256="00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444"
        LINUXDEPLOY_PLUGIN_SHA256="83c292149274965a865dcd44c135cfca8ba28c6b7de3eb628d4b8b5f248af17c"
        ;;
esac

LINUXDEPLOY_URL="${LINUXDEPLOY_URL:-https://github.com/linuxdeploy/linuxdeploy/releases/download/${LINUXDEPLOY_RELEASE}/linuxdeploy-${linuxdeploy_arch}.AppImage}"
LINUXDEPLOY_PLUGIN_URL="${LINUXDEPLOY_PLUGIN_URL:-https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/${LINUXDEPLOY_PLUGIN_RELEASE}/linuxdeploy-plugin-appimage-${linuxdeploy_arch}.AppImage}"
APPIMAGE_RUNTIME_URL="https://github.com/AppImage/type2-runtime/releases/download/${APPIMAGE_RUNTIME_RELEASE}/runtime-${linuxdeploy_arch}"
GTK_PLUGIN_URL="${GTK_PLUGIN_URL:-https://raw.githubusercontent.com/tauri-apps/tauri/${TAURI_GTK_PLUGIN_COMMIT}/crates/tauri-bundler/src/bundle/linux/appimage/linuxdeploy-plugin-gtk.sh}"

LINUXDEPLOY_APPIMAGE="$TOOLS_ROOT/linuxdeploy-${linuxdeploy_arch}.AppImage"
LINUXDEPLOY_ROOT="$TOOLS_ROOT/linuxdeploy-${linuxdeploy_arch}.root"
LINUXDEPLOY_BIN="$LINUXDEPLOY_ROOT/usr/bin/linuxdeploy"
LINUXDEPLOY_PLUGIN_APPIMAGE="$TOOLS_ROOT/linuxdeploy-plugin-appimage-${linuxdeploy_arch}.AppImage"
LINUXDEPLOY_PLUGIN_BIN="$TOOLS_ROOT/linuxdeploy-plugin-appimage"
APPIMAGE_RUNTIME_FILE="$TOOLS_ROOT/appimage-runtime-${linuxdeploy_arch}"
APPIMAGE_RUNTIME_SIG="$APPIMAGE_RUNTIME_FILE.sig"
GTK_PLUGIN_SOURCE="$TOOLS_ROOT/linuxdeploy-plugin-gtk.pinned.sh"
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

download_verified() {
    local url="$1"
    local output="$2"
    local expected="$3"
    local actual

    download_if_missing "$url" "$output"
    actual="$(sha256_file "$output")" || { rm -f "$output"; echo "no sha256 tool available to verify $output" >&2; exit 1; }
    if [[ "$actual" != "$expected" ]]; then
        rm -f "$output"
        echo "sha256 mismatch for $output: got $actual, expected $expected" >&2
        exit 1
    fi
}

verify_appimage_runtime_signature() {
    command -v gpg >/dev/null 2>&1 || return 1
    [[ -s "$APPIMAGE_RUNTIME_SIG" ]] || return 1
    gpg --list-keys "$APPIMAGE_RUNTIME_GPG_KEY" >/dev/null 2>&1 \
        || gpg --keyserver hkps://keys.openpgp.org --recv-keys "$APPIMAGE_RUNTIME_GPG_KEY" >/dev/null 2>&1 \
        || return 1
    gpg --verify "$APPIMAGE_RUNTIME_SIG" "$APPIMAGE_RUNTIME_FILE" >/dev/null 2>&1
}

extract_linuxdeploy() {
    local extract_dir="$TOOLS_ROOT/.linuxdeploy-extract"

    rm -rf "$extract_dir" "$LINUXDEPLOY_ROOT"
    mkdir -p "$extract_dir"
    (
        cd "$extract_dir"
        "$LINUXDEPLOY_APPIMAGE" --appimage-extract >/dev/null
    )
    mv "$extract_dir/squashfs-root" "$LINUXDEPLOY_ROOT"
    rm -rf "$extract_dir"


    rm -f "$LINUXDEPLOY_ROOT/usr/bin/strip"
    ln -s /usr/bin/strip "$LINUXDEPLOY_ROOT/usr/bin/strip"
}

mkdir -p "$DIST_ROOT" "$TOOLS_ROOT"

render_packaged_app_meta() {
    local output="$DIST_ROOT/.${APP_BINARY}-app-meta.sh"
    {
        printf '#!/usr/bin/env bash\n'
        for name in \
            LSB_BUILD_PROFILE APP_ID APP_ICON_NAME APP_ICON_SOURCE_ID APP_ICON_SOURCE_NAME \
            APP_BINARY APP_NAME APP_COMMENT APP_URL APP_REPO UPDATE_CHANNEL CONFIG_DIR_NAME \
            STATE_DIR_NAME ENGINE_SERVICE_NAME ENGINE_TARGET_NAME PIPEWIRE_CONF_NAME \
            PIPEWIRE_NAMESPACE VIRTUAL_OUTPUT_DESCRIPTION VIRTUAL_MIC_DESCRIPTION \
            LSB_ALLOW_PRIVILEGED_HELPER LSB_ALLOW_DEFAULT_SOURCE_CLAIM HOTKEY_PIPE_NAME LOCAL_PLAYBACK_NODE_NAME MIC_CAPTURE_NODE_NAME \
            VIRTUAL_SOURCE_NAME VIRTUAL_MIC_FEEDER_NODE_NAME MANAGED_MARKER; do
            printf '%s=%q\n' "$name" "${!name}"
        done
    } >"$output"
    chmod 644 "$output"
    APP_META_RENDERED="$output"
}
render_packaged_app_meta

if [[ "$build_project" -eq 1 ]]; then
    RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }--remap-path-prefix=${REPO_ROOT}=."
    if [[ -n "${HOME:-}" ]]; then
        RUSTFLAGS="${RUSTFLAGS} --remap-path-prefix=${HOME}=~"
    fi
    export RUSTFLAGS
    cargo build --locked --release --manifest-path "$MANIFEST_PATH"
fi

if [[ ! -x "$CARGO_BINARY_SOURCE" ]]; then
    echo "Expected built binary at $CARGO_BINARY_SOURCE" >&2
    exit 1
fi
if [[ "$APP_BINARY" != "linux-soundboard" ]]; then
    profile_bin_dir="$DIST_ROOT/.profile-bin"
    mkdir -p "$profile_bin_dir"
    BINARY_SOURCE="$profile_bin_dir/$APP_BINARY"
    install -m755 "$CARGO_BINARY_SOURCE" "$BINARY_SOURCE"
else
    BINARY_SOURCE="$CARGO_BINARY_SOURCE"
fi

download_verified "$LINUXDEPLOY_URL" "$LINUXDEPLOY_APPIMAGE" "$LINUXDEPLOY_SHA256"
download_verified "$LINUXDEPLOY_PLUGIN_URL" "$LINUXDEPLOY_PLUGIN_APPIMAGE" "$LINUXDEPLOY_PLUGIN_SHA256"
download_verified "$APPIMAGE_RUNTIME_URL" "$APPIMAGE_RUNTIME_FILE" "$APPIMAGE_RUNTIME_SHA256"
download_verified "$GTK_PLUGIN_URL" "$GTK_PLUGIN_SOURCE" "$GTK_PLUGIN_SHA256"
cp "$GTK_PLUGIN_SOURCE" "$GTK_PLUGIN_BIN"

chmod +x "$LINUXDEPLOY_APPIMAGE" "$GTK_PLUGIN_BIN" "$LINUXDEPLOY_PLUGIN_APPIMAGE"
cp "$LINUXDEPLOY_PLUGIN_APPIMAGE" "$LINUXDEPLOY_PLUGIN_BIN"
extract_linuxdeploy

if curl -fsSL "$APPIMAGE_RUNTIME_URL.sig" -o "$APPIMAGE_RUNTIME_SIG" 2>/dev/null \
    && verify_appimage_runtime_signature; then
    echo "✓ AppImage runtime signature verified against $APPIMAGE_RUNTIME_GPG_KEY"
else
    rm -f "$APPIMAGE_RUNTIME_SIG"
    echo "AppImage runtime signature not verified (gpg or the signing key is unavailable); the pinned sha256 is the anchor"
fi

export LDAI_RUNTIME_FILE="$APPIMAGE_RUNTIME_FILE"



if ! grep -q 'DEPLOY_GTK_VERSION="${DEPLOY_GTK_VERSION:-4}"' "$GTK_PLUGIN_BIN"; then

    sed -i 's/^DEPLOY_GTK_VERSION=3.*/DEPLOY_GTK_VERSION="${DEPLOY_GTK_VERSION:-4}"/' "$GTK_PLUGIN_BIN"
fi

if grep -q 'find /usr/lib\* -name libgiognutls.so' "$GTK_PLUGIN_BIN"; then
    sed -i 's|find /usr/lib\\* -name libgiognutls.so|find /usr/lib -name libgiognutls.so|' "$GTK_PLUGIN_BIN"
fi



if grep -Fq 'copy_tree "$gtk4_libdir" "$APPDIR/"' "$GTK_PLUGIN_BIN"; then

    sed -i 's|^\( *\)copy_tree "\$gtk4_libdir" "\$APPDIR/"|\1if [ -d "$gtk4_libdir" ]; then copy_tree "$gtk4_libdir" "$APPDIR/"; fi|' "$GTK_PLUGIN_BIN"
fi



if grep -Fq 'done < <(find "$directory" \( -type l -o -type f \) -name "$library" -print0)' "$GTK_PLUGIN_BIN"; then

    sed -i '/done < <(find "\$directory" \\( -type l -o -type f \\) -name "\$library" -print0)/c\    done < <(find "$directory" \\( -type l -o -type f \\) ! -path "/usr/lib/vmware/*" ! -path "/usr/lib/vmware/**" -name "$library" -print0)' "$GTK_PLUGIN_BIN"
fi

echo "Patching GTK plugin for Wayland support..."
if grep -q 'export GDK_BACKEND=x11' "$GTK_PLUGIN_BIN"; then
    sed -i '/export GDK_BACKEND=x11/c\
if [ -z "${GDK_BACKEND:-}" ]; then\
    if [ -n "${WAYLAND_DISPLAY:-}" ] \&\& [ -z "${LSB_FORCE_X11:-}" ]; then\
        export GDK_BACKEND=wayland\
    elif [ -n "${DISPLAY:-}" ]; then\
        export GDK_BACKEND=x11\
    fi\
fi' "$GTK_PLUGIN_BIN"
    echo "✓ Wayland support enabled in GTK plugin"
fi

rm -rf "$APPDIR"
rm -f "$versioned_path" "$stable_path"
mkdir -p \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/doc/$APP_BINARY" \
    "$APPDIR/usr/share/metainfo" \
    "$APPDIR/usr/libexec/$APP_BINARY" \
    "$APPDIR/usr/libexec/$APP_BINARY/installer/icons"

if [[ "$LSB_ALLOW_PRIVILEGED_HELPER" == "1" ]]; then
    install -Dm755 "$SWHKD_HELPER_SOURCE" "$APPDIR/usr/libexec/$APP_BINARY/install-swhkd-helper.sh"
    install -Dm755 "$SWHKD_BUILD_SCRIPT_SOURCE" "$APPDIR/usr/libexec/$APP_BINARY/build-swhkd-locked.sh"
    install -Dm644 "$SWHKD_PINNED_LOCK_SOURCE" "$APPDIR/usr/libexec/$APP_BINARY/swhkd-Cargo.lock.pinned"
    install -Dm755 "$SWHKD_HELPER_SOURCE" "$APPDIR/usr/bin/install-swhkd-helper.sh"
fi
install -Dm755 "$INSTALLER_SOURCE" "$APPDIR/usr/libexec/$APP_BINARY/installer/install-user.sh"
install -Dm644 "$APP_META_RENDERED" "$APPDIR/usr/libexec/$APP_BINARY/installer/app-meta.sh"
if [[ "$LSB_ALLOW_PRIVILEGED_HELPER" == "1" ]]; then
    install -Dm755 "$SWHKD_HELPER_SOURCE" "$APPDIR/usr/libexec/$APP_BINARY/installer/install-swhkd-helper.sh"
    install -Dm755 "$SWHKD_BUILD_SCRIPT_SOURCE" "$APPDIR/usr/libexec/$APP_BINARY/installer/build-swhkd-locked.sh"
    install -Dm644 "$SWHKD_PINNED_LOCK_SOURCE" "$APPDIR/usr/libexec/$APP_BINARY/installer/swhkd-Cargo.lock.pinned"
fi

for legal_file in LICENSE NOTICE.md THIRDPARTY_LICENSES.md THIRD_PARTY_NOTICES.html COMMERCIAL-LICENSE.md; do
    install -Dm644 "$REPO_ROOT/$legal_file" "$APPDIR/usr/share/doc/$APP_BINARY/$legal_file"
    install -Dm644 "$REPO_ROOT/$legal_file" "$APPDIR/usr/libexec/$APP_BINARY/installer/$legal_file"
done

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
done < <(find "$ICON_SOURCE_ROOT" -path "*/apps/$APP_ICON_SOURCE_ID.png" -type f | sort)

while IFS= read -r icon_path; do
    relative_path="${icon_path#"$ICON_SOURCE_ROOT/"}"
    install -Dm644 "$icon_path" "$APPDIR/usr/libexec/$APP_BINARY/installer/icons/$relative_path"
done < <(find "$ICON_SOURCE_ROOT" -type f | sort)


APP_ICON_SIZES=(16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512)
SYMBOLIC_CONTEXTS=(actions:Actions devices:Devices places:Places)
{
    printf '[Icon Theme]\nName=Hicolor\nComment=Fallback icon theme\nHidden=true\nDirectories='
    directories=()
    for size in "${APP_ICON_SIZES[@]}"; do
        directories+=("$size/apps")
    done
    for context in "${SYMBOLIC_CONTEXTS[@]}"; do
        directories+=("scalable/${context%%:*}")
    done
    printf '%s' "$(IFS=,; printf '%s' "${directories[*]}")"
    printf '\n\n'
    for size in "${APP_ICON_SIZES[@]}"; do
        printf '[%s/apps]\nSize=%s\nContext=Applications\nType=Fixed\n\n' \
            "$size" "${size%%x*}"
    done
    for context in "${SYMBOLIC_CONTEXTS[@]}"; do
        printf '[scalable/%s]\nSize=16\nMinSize=8\nMaxSize=512\nContext=%s\nType=Scalable\n\n' \
            "${context%%:*}" "${context##*:}"
    done
} >"$APPDIR/usr/share/icons/hicolor/index.theme"

DEPLOY_PATH="$TOOLS_ROOT:$LINUXDEPLOY_ROOT/usr/bin:$PATH"

(
    cd "$DIST_ROOT"
    DEPLOY_GTK_VERSION=4 PATH="$DEPLOY_PATH" "$LINUXDEPLOY_BIN" \
        --appdir "$APPDIR" \
        --executable "$BINARY_SOURCE" \
        --desktop-file "$APPDIR/usr/share/applications/$APP_ID.desktop" \
        --icon-file "$ICON_SOURCE_ROOT/512x512/apps/$APP_ICON_SOURCE_NAME.png" \
        --plugin gtk
)

rm -rf "$APPDIR/usr/lib32"



gtk_hook="$APPDIR/apprun-hooks/linuxdeploy-plugin-gtk.sh"
if [[ -f "$gtk_hook" ]]; then


    sed -i '/^export GIO_EXTRA_MODULES=/,/lib32\/gio\/modules"$/c\export GIO_EXTRA_MODULES="$APPDIR/usr/lib/gio/modules"' "$gtk_hook"




    if ! grep -q '^export GTK_THEME="\$APPIMAGE_GTK_THEME"' "$gtk_hook"; then
        echo "Expected GTK_THEME export missing from $gtk_hook; check GTK_PLUGIN_URL" >&2
        exit 1
    fi


    sed -i 's|^export GTK_THEME="\$APPIMAGE_GTK_THEME".*|if [ -n "${LSB_GTK_THEME:-}" ]; then export GTK_THEME="$LSB_GTK_THEME"; fi|' "$gtk_hook"
fi


echo "Skipping pactl/wpctl bundling; the runtime uses host PipeWire/PulseAudio services."

echo "Removing unnecessary libraries..."


rm -f "$APPDIR/usr/lib"/libopenraw* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libglycin* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libdav1d* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libavif* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libheif* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libjxl* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libde265* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libx265* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libkvazaar* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libSvtAv1* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/libaom* 2>/dev/null || true
rm -f "$APPDIR/usr/lib"/librav1e* 2>/dev/null || true

echo "Library cleanup complete"

echo "Adding preflight dependency checker..."
install -Dm755 "$SCRIPT_DIR/appimage-preflight-check.sh" "$APPDIR/usr/bin/appimage-preflight-check"


[ -f "$gtk_hook" ] || { echo "no $gtk_hook to hang the preflight check on" >&2; exit 1; }
cat >>"$gtk_hook" <<'HOOK'


if [ -z "$SKIP_PREFLIGHT_CHECK" ]; then
    "$APPDIR"/usr/bin/appimage-preflight-check || exit 1
fi
HOOK

(
    cd "$DIST_ROOT"
    PATH="$DEPLOY_PATH" ARCH="$linuxdeploy_arch" LDAI_OUTPUT="$versioned_name" \
        "$LINUXDEPLOY_BIN" \
        --appdir "$APPDIR" \
        --output appimage
)


if ! grep -q 'appimage-preflight-check' "$gtk_hook" \
    || ! grep -q "apprun-hooks/\"linuxdeploy-plugin-gtk.sh\"" "$APPDIR/AppRun"; then
    echo "the preflight check is not reachable from AppRun; it would never run" >&2
    exit 1
fi
echo "✓ Preflight checker wired into AppRun"

cp "$versioned_path" "$stable_path"
chmod 755 "$versioned_path" "$stable_path"

"$REPO_ROOT/packaging/generate-checksums.sh" "$DIST_ROOT" --only "$versioned_name" --only "$stable_name" >/dev/null

echo "Created AppImage artifacts:"
echo "  Versioned: $versioned_path"
echo "  Stable:    $stable_path"
