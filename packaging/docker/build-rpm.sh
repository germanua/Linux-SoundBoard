#!/usr/bin/env bash
#
# Build the .rpm in a Fedora container, for hosts that are not Fedora/RHEL.
#
# The RPM spec relies on Fedora-only build macros (notably %{_userunitdir} from
# systemd-rpm-macros) and an rpm database, so packaging/rpm/package-rpm.sh cannot
# run on other distributions. This wrapper runs that script inside fedora:latest.
#
# Usage (from anywhere in the checkout):
#   packaging/docker/build-rpm.sh
#
# The resulting .rpm is copied into dist/ at the repository root. Requires docker
# and rsync on the host; the container needs network access to install packages.

set -euo pipefail

IMAGE="${RPM_BUILD_IMAGE:-fedora:latest}"

# ---------------------------------------------------------------------------
# Container stage: install build dependencies and build the RPM into /src/dist.
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# Host stage: stage an isolated build context and run this script in the image.
# ---------------------------------------------------------------------------
command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }
command -v rsync  >/dev/null || { echo "rsync is required"  >&2; exit 1; }

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"

CTX="$(mktemp -d)"
trap 'rm -rf "$CTX"' EXIT
# Copy the working tree (so uncommitted packaging fixes are included) without the
# host build outputs, which are distro-specific and must not leak into the image.
rsync -a \
    --exclude='target/' --exclude='dist/' --exclude='.git/' --exclude='.history/' \
    "$REPO_ROOT"/ "$CTX"/
mkdir -p "$CTX/dist"

echo "==> Running RPM build in $IMAGE"
docker run --rm \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$CTX":/src \
    "$IMAGE" bash /src/packaging/docker/build-rpm.sh --in-container

mkdir -p "$REPO_ROOT/dist"
cp "$CTX"/dist/*.rpm "$REPO_ROOT/dist/"
# package-rpm.sh hashed the container's dist/, not this one, so the host list is
# still missing the rpm we just copied in.
"$REPO_ROOT/packaging/generate-checksums.sh" "$REPO_ROOT/dist" >/dev/null

echo "==> Done. Artifacts in $REPO_ROOT/dist:"
ls -1 "$REPO_ROOT"/dist/*.rpm "$REPO_ROOT"/dist/SHA256SUMS.txt
