#!/usr/bin/env bash

set -euo pipefail

UBUNTU_IMAGE_DIGEST="docker.io/library/ubuntu:24.04@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3"
IMAGE="${APPIMAGE_BUILD_IMAGE:-${DEB_BUILD_IMAGE:-$UBUNTU_IMAGE_DIGEST}}"

if [ "${1:-}" = "--in-container" ]; then
    export DEBIAN_FRONTEND=noninteractive
    export APPIMAGE_EXTRACT_AND_RUN=1
    . /src/packaging/common.sh
    HOST_UID="${HOST_UID:-0}"
    HOST_GID="${HOST_GID:-0}"

    echo "==> Installing AppImage build dependencies (apt)"
    apt_update() {
        apt-get -o Acquire::Retries=5 update -qq
    }
    apt_install() {
        apt-get -o Acquire::Retries=5 install -y --fix-missing --no-install-recommends \
            build-essential \
            libgtk-4-dev libadwaita-1-dev libpulse-dev libopus-dev \
            libpipewire-0.3-dev libx11-dev libxi-dev pkg-config \
            clang libclang-dev \
            librsvg2-dev librsvg2-common libgdk-pixbuf-2.0-dev librsvg2-2 \
            curl ca-certificates file libfuse2t64 desktop-file-utils patchelf zsync gnupg >/dev/null
    }
    apt_update
    if ! apt_install; then
        echo "==> Package mirror changed during install; refreshing apt indexes and retrying"
        apt-get clean
        rm -rf /var/lib/apt/lists/*
        apt_update
        apt_install
    fi

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
    rustup_actual="$(sha256_file /tmp/rustup-init)" || { echo "FATAL: no sha256 tool available" >&2; exit 3; }
    [ "$rustup_actual" = "$rustup_sha" ] \
        || { echo "FATAL: rustup-init sha256 is $rustup_actual, expected $rustup_sha" >&2; exit 3; }
    chmod +x /tmp/rustup-init
    /tmp/rustup-init -y --default-toolchain "$RUST_TOOLCHAIN" --profile minimal --no-modify-path >/dev/null
    . "$HOME/.cargo/env"
    echo "==> Toolchain: $(rustc --version)"

    cd /src
    OUTPUT_DIR="${LSB_DIST_ROOT:-/src/dist}"
    mkdir -p "$OUTPUT_DIR"
    export CARGO_TARGET_DIR=/tmp/linux-soundboard-target
    trap 'chown -R "$HOST_UID:$HOST_GID" "$OUTPUT_DIR" 2>/dev/null || true' EXIT

    echo "==> Building portable AppImage (${LSB_BUILD_PROFILE:-stable})"
    bash packaging/linux/package-appimage.sh
    exit 0
fi

command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }
command -v rsync  >/dev/null || { echo "rsync is required" >&2; exit 1; }

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
CTX="$(mktemp -d)"
trap 'rm -rf "$CTX"' EXIT

PROFILE="${LSB_BUILD_PROFILE:-stable}"
if [[ "$PROFILE" == "dev" ]]; then
    [[ "${LSB_DEV_VERSION:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+-dev\.[0-9]+$ ]] \
        || { echo "LSB_DEV_VERSION=X.Y.Z-dev.N is required for DEV AppImage builds" >&2; exit 1; }
    OUTPUT_NAME="dev/dist"
    PRIVATE_CONTEXT_EXCLUDES=(--exclude='dev/dist/' --exclude='dev/reports/')
else
    OUTPUT_NAME="dist"
    PRIVATE_CONTEXT_EXCLUDES=(--exclude='dev/')
fi

rsync -a \
    --exclude='target/' --exclude='dist/' --exclude='dist-dev/' --exclude='.git/' --exclude='.history/' \
    --exclude='.commandcode/' "${PRIVATE_CONTEXT_EXCLUDES[@]}" \
    "$REPO_ROOT"/ "$CTX"/
mkdir -p "$CTX/$OUTPUT_NAME"

echo "==> Running AppImage build in $IMAGE ($PROFILE profile)"
docker run --rm \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -e LSB_BUILD_PROFILE="$PROFILE" \
    -e LSB_DEV_VERSION="${LSB_DEV_VERSION:-}" \
    -e LSB_DIST_ROOT="/src/$OUTPUT_NAME" \
    -v "$CTX":/src \
    "$IMAGE" bash /src/packaging/docker/build-appimage.sh --in-container

HOST_OUTPUT="$REPO_ROOT/$OUTPUT_NAME"
mkdir -p "$HOST_OUTPUT"
shopt -s nullglob
built=("$CTX"/"$OUTPUT_NAME"/*.AppImage)
shopt -u nullglob
[ "${#built[@]}" -gt 0 ] || { echo "no AppImage was produced" >&2; exit 1; }

built_names=()
for built_image in "${built[@]}"; do
    built_name="$(basename "$built_image")"
    cp --remove-destination "$built_image" "$HOST_OUTPUT/$built_name"
    chmod 755 "$HOST_OUTPUT/$built_name"
    built_names+=(--only "$built_name")
done
LSB_DIST_ROOT="$HOST_OUTPUT" "$REPO_ROOT/packaging/generate-checksums.sh" "$HOST_OUTPUT" "${built_names[@]}" >/dev/null

echo "==> Done. AppImage artifacts in $HOST_OUTPUT:"
ls -1 "$HOST_OUTPUT"/*.AppImage "$HOST_OUTPUT/SHA256SUMS.txt"
