# Installation Guide

Linux Soundboard supports native Linux packaging, portable AppImage usage, and source builds. This guide is for end users who want to install and run the application on a real system.

## Choose a Path

| System | Recommended path | Result |
| --- | --- | --- |
| Arch Linux / CachyOS / EndeavourOS | AUR | Managed package |
| Ubuntu / Debian | GitHub release `.deb` | Native package |
| Fedora | GitHub release `.rpm` | Native package |
| openSUSE / other x86_64 distributions | AppImage | Portable package |
| Development workstation | Source build | Local binary |

## GitHub Release Packages

The release page is the canonical source for packaged builds:

- https://github.com/germanua/Linux-SoundBoard/releases/latest

### Ubuntu and Debian

Download the current `.deb` package from the release page, then install it with APT so dependencies are resolved automatically:

```bash
sudo apt install ./linux-soundboard_1.1.1-1_amd64.deb
```

Runtime packages commonly involved on Debian-based systems:

- `pipewire`
- `pipewire-pulse`
- `wireplumber`
- `pulseaudio-utils`

### Fedora

Download the current `.rpm` package from the release page, then install it with DNF:

```bash
sudo dnf install ./linux-soundboard-1.1.1-1.x86_64.rpm
```

Runtime packages commonly involved on Fedora:

- `pipewire`
- `pipewire-pulseaudio`
- `wireplumber`
- `pulseaudio-utils`

### AppImage

Use the AppImage when you want a portable build or your distro is not covered by a native package release:

```bash
chmod +x linux-soundboard-x86_64.AppImage
./linux-soundboard-x86_64.AppImage
```

If the AppImage reports a FUSE error, install the matching host package:

- Ubuntu / Debian: `sudo apt install libfuse2`
- Fedora: `sudo dnf install fuse-libs`
- Arch Linux: `sudo pacman -S fuse2`
- openSUSE: `sudo zypper install fuse`

## Arch Linux

Install from the AUR:

```bash
yay -S linux-soundboard-git
```

If you use `paru`:

```bash
paru -S linux-soundboard-git
```

## One-Command Bootstrap

If you have the repository checked out locally, `scripts/install.sh` can detect the current distro, install missing runtime packages, and provision `swhkd` for Wayland sessions:

```bash
cd LinuxSoundBoardv1
./scripts/install.sh
```

What the script currently does:

- Arch family: installs `linux-soundboard-git` from the AUR
- Debian and Ubuntu: installs the latest published `.deb`, or falls back to the AppImage plus runtime packages if no `.deb` is available
- Fedora: installs the latest published `.rpm`, or falls back to the AppImage plus runtime packages if no `.rpm` is available
- openSUSE and SUSE family: installs the AppImage plus the host runtime packages it needs
- Wayland sessions: installs and configures `swhkd`
- X11 sessions: skips `swhkd` because the native X11 backend is used instead

## Wayland, X11, and Hotkeys

Hotkeys depend on the session type:

- Wayland: install `swhkd`
- X11 / XWayland: use the built-in X11 backend

`swhkd` packaging differs by distro:

- Arch family: `swhkd-bin` or `swhkd-git` from the AUR
- Debian, Ubuntu, Fedora, openSUSE: build or install `swhkd` from upstream

Upstream installation notes:

- https://github.com/waycrate/swhkd/blob/main/INSTALL.md

## First Launch Checklist

1. Launch `linux-soundboard`.
2. Add a sound folder or drag files into the window.
3. Confirm PipeWire is running:

```bash
systemctl --user status pipewire pipewire-pulse wireplumber
```

4. In Discord, OBS, Zoom, or another target application, choose `Linux_Soundboard_Mic` as the input device.
5. If you need your real microphone mixed in, enable mic passthrough in the app settings.

## Build From Source

### Debian and Ubuntu

```bash
sudo apt install build-essential cargo rustc pkg-config imagemagick \
  libgtk-4-dev libadwaita-1-dev libpulse-dev libasound2-dev \
  libx11-dev libxi-dev pipewire pipewire-pulse wireplumber pulseaudio-utils
```

### Fedora

```bash
sudo dnf install cargo rust gcc gcc-c++ clang pkg-config ImageMagick \
  gtk4-devel libadwaita-devel pulseaudio-libs-devel alsa-lib-devel \
  libX11-devel libXi-devel pipewire pipewire-pulseaudio wireplumber pulseaudio-utils
```

### Arch Linux

```bash
sudo pacman -S cargo rust pkgconf imagemagick gtk4 libadwaita libpulse \
  alsa-lib libx11 libxi pipewire pipewire-pulse wireplumber
```

### Build and Run

```bash
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard/src
cargo build --release
./target/release/linux-soundboard
```

## Flatpak

The repository contains Flatpak packaging files, but the project does not currently publish an end-user Flathub install target.

## Next Step

If the app installs but does not behave correctly on your system, go to [TROUBLESHOOTING.md](TROUBLESHOOTING.md).
