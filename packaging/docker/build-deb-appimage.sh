#!/usr/bin/env bash

















set -euo pipefail

UBUNTU_IMAGE_DIGEST="docker.io/library/ubuntu:24.04@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3"
IMAGE="${DEB_BUILD_IMAGE:-$UBUNTU_IMAGE_DIGEST}"




if [ "${1:-}" = "--in-container" ]; then
    export DEBIAN_FRONTEND=noninteractive
    export APPIMAGE_EXTRACT_AND_RUN=1
    HOST_UID="${HOST_UID:-0}"
    HOST_GID="${HOST_GID:-0}"

    echo "==> Installing build dependencies (apt)"
    apt-get update -qq
    apt-get install -y --no-install-recommends \
        debhelper dpkg-dev fakeroot build-essential cargo rustc \
        libgtk-4-dev libadwaita-1-dev libpulse-dev libopus-dev \
        libpipewire-0.3-dev libx11-dev libxi-dev pkg-config \
        clang libclang-dev \
        librsvg2-dev librsvg2-common libgdk-pixbuf-2.0-dev librsvg2-2 \
        curl ca-certificates file libfuse2t64 desktop-file-utils patchelf zsync >/dev/null

    RUST_TOOLCHAIN="$(sed -n 's/^channel = "\(.*\)"$/\1/p' /src/rust-toolchain.toml | head -n 1)"
    [ -n "$RUST_TOOLCHAIN" ] || { echo "FATAL: rust-toolchain.toml has no channel" >&2; exit 3; }
    case "$(uname -m)" in
        x86_64) rustup_target="x86_64-unknown-linux-gnu"
                rustup_sha="dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71" ;;
        aarch64|arm64) rustup_target="aarch64-unknown-linux-gnu"
                rustup_sha="15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433" ;;
        *) echo "FATAL: unsupported architecture for rustup: $(uname -m)" >&2; exit 3 ;;
    esac
    echo "==> Installing Rust $RUST_TOOLCHAIN via rustup 1.29.1"
    curl -fsSL "https://static.rust-lang.org/rustup/archive/1.29.1/${rustup_target}/rustup-init" -o /tmp/rustup-init
    rustup_actual="$(sha256sum /tmp/rustup-init | awk '{print $1}')"
    [ "$rustup_actual" = "$rustup_sha" ] \
        || { echo "FATAL: rustup-init sha256 is $rustup_actual, expected $rustup_sha" >&2; exit 3; }
    chmod +x /tmp/rustup-init
    /tmp/rustup-init -y --default-toolchain "$RUST_TOOLCHAIN" --profile minimal --no-modify-path >/dev/null
    PATH="$HOME/.cargo/bin:$PATH"
    export PATH
    echo "==> Toolchain: $(rustc --version)"

    cd /src
    mkdir -p dist



    trap 'chown -R "$HOST_UID:$HOST_GID" /src/dist /src/target 2>/dev/null || true' EXIT

    echo "==> Building .deb"
    rm -rf debian && mkdir -p debian && cp -a packaging/debian/. debian/
    dpkg-buildpackage -us -uc -b
    mv ../*.deb dist/ 2>/dev/null || true
    rm -rf debian ../*.buildinfo ../*.changes

    echo "==> Building portable AppImage"
    bash packaging/linux/package-appimage.sh

    exit 0
fi




command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }
command -v rsync  >/dev/null || { echo "rsync is required"  >&2; exit 1; }

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"

CTX="$(mktemp -d)"
trap 'rm -rf "$CTX"' EXIT
rsync -a \
    --exclude='target/' --exclude='dist/' --exclude='.git/' --exclude='.history/' \
    --exclude='.commandcode/' --exclude='dev/' \
    "$REPO_ROOT"/ "$CTX"/
mkdir -p "$CTX/dist"

echo "==> Running deb + AppImage build in $IMAGE"
docker run --rm \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$CTX":/src \
    "$IMAGE" bash /src/packaging/docker/build-deb-appimage.sh --in-container

mkdir -p "$REPO_ROOT/dist"
cp --remove-destination "$CTX"/dist/*.deb "$CTX"/dist/*.AppImage "$REPO_ROOT/dist/"
chmod 755 "$REPO_ROOT/dist/"*.AppImage
"$REPO_ROOT/packaging/generate-checksums.sh" "$REPO_ROOT/dist" >/dev/null

echo "==> Done. Artifacts in $REPO_ROOT/dist:"
ls -1 "$REPO_ROOT"/dist/*.deb "$REPO_ROOT"/dist/*.AppImage "$REPO_ROOT"/dist/SHA256SUMS.txt
