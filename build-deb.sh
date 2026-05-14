#!/usr/bin/env bash
set -euo pipefail
apt-get update
apt-get install -y \
    dpkg-dev debhelper \
    libgtk-4-dev libadwaita-1-dev \
    libpulse-dev libasound2-dev \
    libx11-dev libxi-dev \
    pkg-config imagemagick git \
    curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
source "$HOME/.cargo/env"
cd /repo
./packaging/debian/package-deb.sh
