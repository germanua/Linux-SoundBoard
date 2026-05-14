#!/usr/bin/env bash
set -euo pipefail
dnf install -y \
    rpm-build rust cargo \
    gtk4-devel libadwaita-devel \
    pulseaudio-libs-devel alsa-lib-devel pipewire-devel \
    libX11-devel libXi-devel \
    pkgconf-pkg-config \
    ImageMagick git
cd /repo
./packaging/rpm/package-rpm.sh
