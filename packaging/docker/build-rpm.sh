#!/usr/bin/env bash












set -euo pipefail

FEDORA_IMAGE_DIGEST="docker.io/library/fedora:44@sha256:43b29f65a41eb9c35e1cd5323e3bdf3b655c2357a9f4f1ff2f9c2798e5045d80"
IMAGE="${RPM_BUILD_IMAGE:-$FEDORA_IMAGE_DIGEST}"




if [ "${1:-}" = "--in-container" ]; then
    HOST_UID="${HOST_UID:-0}"
    HOST_GID="${HOST_GID:-0}"

    echo "==> Installing build dependencies (dnf)"
    dnf -y --setopt=install_weak_deps=False install \
        rpm-build rpmdevtools tar gzip findutils \
        cargo rust clang-devel \
        gtk4-devel libadwaita-devel pulseaudio-libs-devel \
        opus-devel pipewire-devel libX11-devel libXi-devel \
        pkgconf-pkg-config systemd-rpm-macros >/dev/null

    echo "==> Toolchain: $(rustc --version), $(cargo --version)"

    echo "==> Building RPM via packaging/rpm/package-rpm.sh"
    cd /src
    bash packaging/rpm/package-rpm.sh

    chown -R "$HOST_UID:$HOST_GID" /src/dist 2>/dev/null || true
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

echo "==> Running RPM build in $IMAGE"
docker run --rm \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$CTX":/src \
    "$IMAGE" bash /src/packaging/docker/build-rpm.sh --in-container

mkdir -p "$REPO_ROOT/dist"
cp "$CTX"/dist/*.rpm "$REPO_ROOT/dist/"


"$REPO_ROOT/packaging/generate-checksums.sh" "$REPO_ROOT/dist" >/dev/null

echo "==> Done. Artifacts in $REPO_ROOT/dist:"
ls -1 "$REPO_ROOT"/dist/*.rpm "$REPO_ROOT"/dist/SHA256SUMS.txt
