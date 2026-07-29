#!/usr/bin/env bash
#
# Build the .deb and a portable AppImage in an Ubuntu 24.04 container, for hosts
# that cannot produce them natively.
#
# The .deb needs a Debian toolchain (dpkg-buildpackage, debhelper) and the AppImage
# must be built against an older glibc than a rolling-release host provides, or the
# bundled GTK stack crashes the dynamic loader at startup. Ubuntu 24.04 supplies
# glibc 2.39 and GTK 4.14 / libadwaita 1.5, which satisfy the gtk4 "v4_10" and
# libadwaita "v1_5" bindings while keeping the AppImage portable.
#
# Usage (from anywhere in the checkout):
#   packaging/docker/build-deb-appimage.sh
#
# Resulting artifacts are copied into dist/ at the repository root. Requires docker
# and rsync on the host; the container needs network access.

set -euo pipefail

IMAGE="${DEB_BUILD_IMAGE:-ubuntu:24.04}"

# ---------------------------------------------------------------------------
# Container stage: install dependencies and build the .deb and AppImage.
# ---------------------------------------------------------------------------
if [ "${1:-}" = "--in-container" ]; then
    export DEBIAN_FRONTEND=noninteractive
    export APPIMAGE_EXTRACT_AND_RUN=1   # no FUSE inside the container
    HOST_UID="${HOST_UID:-0}"
    HOST_GID="${HOST_GID:-0}"

    echo "==> Installing build dependencies (apt)"
    apt-get update -qq
    apt-get install -y --no-install-recommends \
        debhelper dpkg-dev fakeroot build-essential \
        libgtk-4-dev libadwaita-1-dev libpulse-dev libasound2-dev libopus-dev \
        libpipewire-0.3-dev libx11-dev libxi-dev pkg-config imagemagick \
        clang libclang-dev \
        librsvg2-dev librsvg2-common libgdk-pixbuf-2.0-dev librsvg2-2 \
        curl ca-certificates file libfuse2t64 desktop-file-utils patchelf zsync >/dev/null

    # Ubuntu 24.04 ships ImageMagick 6 (`convert`); generate-icons.sh calls the
    # ImageMagick 7 `magick` command. Its usage is plain input->ops->output, which
    # `convert` accepts unchanged, so provide a shim.
    if ! command -v magick >/dev/null 2>&1; then
        printf '#!/bin/sh\nexec convert "$@"\n' > /usr/local/bin/magick
        chmod +x /usr/local/bin/magick
    fi

    # The crate pins Rust 1.85 (rust-toolchain.toml); apt's rustc is older.
    echo "==> Installing Rust 1.85 via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain 1.85.0 --profile minimal >/dev/null
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
    echo "==> Toolchain: $(rustc --version)"

    cd /src
    mkdir -p dist

    echo "==> Building .deb"
    # Mirror packaging/debian/package-deb.sh, but pass -d: its default
    # dpkg-buildpackage invocation fails dpkg-checkbuilddeps because apt has no
    # `rustc (>= 1.85)` package. rustup provides cargo/rustc on PATH instead, so
    # the build-dependency check is skipped while the actual toolchain is present.
    rm -rf debian && mkdir -p debian && cp -a packaging/debian/. debian/
    dpkg-buildpackage -us -uc -b -d
    mv ../*.deb dist/ 2>/dev/null || true
    rm -rf debian ../*.buildinfo ../*.changes

    echo "==> Building portable AppImage"
    bash packaging/linux/package-appimage.sh

    # Hand every build output (including the root-owned cargo target/) back to the
    # host user so the host can delete the temporary build context afterwards.
    chown -R "$HOST_UID:$HOST_GID" dist target 2>/dev/null || true
    exit 0
fi

# ---------------------------------------------------------------------------
# Host stage: stage an isolated build context and run this script in the image.
# ---------------------------------------------------------------------------
command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }
command -v rsync  >/dev/null || { echo "rsync is required"  >&2; exit 1; }

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"

CTX="$(mktemp -d)"
trap 'rm -rf "$CTX"' EXIT
rsync -a \
    --exclude='target/' --exclude='dist/' --exclude='.git/' --exclude='.history/' \
    "$REPO_ROOT"/ "$CTX"/
mkdir -p "$CTX/dist"

echo "==> Running deb + AppImage build in $IMAGE"
docker run --rm \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$CTX":/src \
    "$IMAGE" bash /src/packaging/docker/build-deb-appimage.sh --in-container

mkdir -p "$REPO_ROOT/dist"
cp "$CTX"/dist/*.deb "$CTX"/dist/*.AppImage "$REPO_ROOT/dist/"
echo "==> Done. Artifacts in $REPO_ROOT/dist:"
ls -1 "$REPO_ROOT"/dist/*.deb "$REPO_ROOT"/dist/*.AppImage
