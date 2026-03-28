# Packaging Guide for Linux Soundboard

This document provides comprehensive instructions for building and maintaining Linux Soundboard packages across different distributions.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Building Packages](#building-packages)
  - [AppImage](#appimage)
  - [DEB (Debian/Ubuntu)](#deb-debianubuntu)
  - [RPM (Fedora/RHEL)](#rpm-fedorarhel)
  - [Flatpak](#flatpak)
- [Testing Packages](#testing-packages)
- [Release Checklist](#release-checklist)
- [Troubleshooting](#troubleshooting)

---

## Prerequisites

### Build System Requirements

- **Operating System**: Linux (preferably Arch, Ubuntu 24.04, or Fedora 40)
- **Rust**: 1.85 or later (via rustup)
- **Git**: For version control
- **ImageMagick**: For icon generation

### Distribution-Specific Tools

| Package Type | Required Tools |
|--------------|----------------|
| AppImage | `linuxdeploy`, `curl` |
| DEB | `dpkg-dev`, `debhelper` (>= 13) |
| RPM | `rpm-build`, `rpmbuild` |
| Flatpak | `flatpak-builder`, `python3` |

### Runtime Dependencies

All packages require these runtime dependencies:
- GTK4 (>= 4.10)
- Libadwaita (>= 1.5)
- PulseAudio libraries
- PipeWire + WirePlumber
- PulseAudio utilities (`pactl`)

---

## Building Packages

### AppImage

**Location**: `packaging/linux/package-appimage.sh`

**Build Command**:
```bash
cd /path/to/Linux-SoundBoard
./packaging/linux/package-appimage.sh
```

**Output**:
- `dist/linux-soundboard-x86_64.AppImage` (stable name)
- `dist/linux-soundboard-1.1.0-x86_64.AppImage` (versioned)

**Build Options**:
```bash
# Skip Rust build (use existing binary)
./packaging/linux/package-appimage.sh --skip-build
```

**What It Does**:
1. Builds the Rust application with `cargo build --release`
2. Generates icons from `icon.png`
3. Downloads and extracts `linuxdeploy` tool
4. Patches GTK plugin for native Wayland support
5. Bundles GTK4, Libadwaita, and dependencies
6. Bundles `pactl` binary for virtual microphone
7. Adds preflight dependency checker
8. Creates Type 2 AppImage with embedded squashfs

**New in v1.1.0**:
- ✅ Full native Wayland and X11 support
- ✅ Bundled `pactl` for virtual mic
- ✅ Automatic dependency checking
- ✅ Smart GTK backend detection

**Troubleshooting**:
- If `pactl` is not found during build, install `pulseaudio-utils`
- If linuxdeploy fails, check internet connection (downloads tools)
- If GTK plugin patching fails, check `dist/.appimage-tools/linuxdeploy-plugin-gtk.sh`

---

### DEB (Debian/Ubuntu)

**Location**: `packaging/debian/`

**Build Command**:
```bash
cd /path/to/Linux-SoundBoard
./packaging/debian/package-deb.sh
```

**Output**:
- `dist/linux-soundboard_1.1.0-1_amd64.deb`

**Package Structure**:
```
packaging/debian/
├── control          # Package metadata and dependencies
├── rules            # Build instructions (Makefile)
├── changelog        # Version history
├── copyright        # License information
├── compat           # Debhelper compatibility level (13)
└── linux-soundboard.desktop  # Desktop entry
```

**What It Does**:
1. Runs `dpkg-buildpackage` to build the package
2. Compiles Rust application
3. Generates icons
4. Installs binary to `/usr/bin/`
5. Installs desktop file and icons
6. Creates `.deb` package with dependency metadata

**Dependencies** (auto-installed):
- libgtk-4-1
- libadwaita-1-0
- libpulse0
- pipewire, pipewire-pulse, wireplumber
- pulseaudio-utils

**Testing**:
```bash
# Install
sudo apt install ./dist/linux-soundboard_1.1.0-1_amd64.deb

# Verify
dpkg -L linux-soundboard
linux-soundboard --version

# Uninstall
sudo apt remove linux-soundboard
```

**Updating Version**:
1. Edit `packaging/debian/changelog`
2. Update version in `src/Cargo.toml`
3. Rebuild package

---

### RPM (Fedora/RHEL)

**Location**: `packaging/rpm/`

**Build Command**:
```bash
cd /path/to/Linux-SoundBoard
./packaging/rpm/package-rpm.sh
```

**Output**:
- `dist/linux-soundboard-1.1.0-1.fc40.x86_64.rpm`

**Package Structure**:
```
packaging/rpm/
├── linux-soundboard.spec     # RPM spec file
├── linux-soundboard.desktop  # Desktop entry
└── package-rpm.sh            # Build script
```

**What It Does**:
1. Creates source tarball from git repository
2. Sets up RPM build directory (`~/rpmbuild/`)
3. Runs `rpmbuild -ba` to build package
4. Compiles Rust application
5. Generates icons
6. Installs files to appropriate locations
7. Creates `.rpm` package with dependency metadata

**Dependencies** (auto-installed):
- gtk4
- libadwaita
- pulseaudio-libs
- pipewire, pipewire-pulseaudio, wireplumber
- pulseaudio-utils

**Testing**:
```bash
# Install
sudo dnf install ./dist/linux-soundboard-1.1.0-1.fc40.x86_64.rpm

# Verify
rpm -ql linux-soundboard
linux-soundboard --version

# Uninstall
sudo dnf remove linux-soundboard
```

**Updating Version**:
1. Edit `packaging/rpm/linux-soundboard.spec` (Version and %changelog)
2. Update version in `src/Cargo.toml`
3. Rebuild package

---

### Flatpak

**Location**: `packaging/flatpak/`

**Build Command**:
```bash
cd /path/to/Linux-SoundBoard/packaging/flatpak
./package-flatpak.sh
```

**Output**:
- `dist/linux-soundboard-1.1.0.flatpak` (single-file bundle)
- `flatpak-repo/` (OSTree repository)

**Package Structure**:
```
packaging/flatpak/
├── com.linuxsoundboard.app.yml          # Flatpak manifest
├── com.linuxsoundboard.app.desktop      # Desktop entry
├── com.linuxsoundboard.app.metainfo.xml # AppStream metadata
├── package-flatpak.sh                   # Build script
├── FLATHUB_SUBMISSION.md                # Flathub guide
└── cargo-sources.json                   # Cargo dependencies (generated)
```

**What It Does**:
1. Checks for Flatpak and flatpak-builder
2. Installs GNOME SDK 47 if needed
3. Generates `cargo-sources.json` from `Cargo.lock`
4. Builds application in sandboxed environment
5. Creates OSTree repository
6. Exports single-file `.flatpak` bundle

**Runtime**: `org.gnome.Platform//47`

**Permissions / Host Requirements**:
- Wayland + X11 display access
- PulseAudio/PipeWire audio access
- File system access (read-only for music/downloads)
- Host-side `swhkd` for native Wayland hotkeys

**Testing**:
```bash
# Install locally
flatpak install ./dist/linux-soundboard-1.1.0.flatpak

# Run
flatpak run com.linuxsoundboard.app

# Uninstall
flatpak uninstall com.linuxsoundboard.app
```

**Flathub Submission**:
See `packaging/flatpak/FLATHUB_SUBMISSION.md` for detailed instructions.

**Updating Version**:
1. Edit `packaging/flatpak/com.linuxsoundboard.app.metainfo.xml` (add release entry)
2. Update version in `src/Cargo.toml`
3. Regenerate `cargo-sources.json`:
   ```bash
   python3 flatpak-cargo-generator.py ../../src/Cargo.lock -o cargo-sources.json
   ```
4. Rebuild package

---

## Testing Packages

### Test Matrix

| Distribution | Version | Desktop | Package Types | Priority |
|--------------|---------|---------|---------------|----------|
| Ubuntu | 24.04 | GNOME (Wayland) | DEB, AppImage, Flatpak | High |
| Ubuntu | 22.04 | GNOME (X11) | DEB, AppImage | Medium |
| Debian | 13 | GNOME | DEB | Medium |
| Fedora | 40 | GNOME (Wayland) | RPM, AppImage, Flatpak | High |
| Fedora | 39 | GNOME (Wayland) | RPM | Medium |
| Arch | Latest | Any | AUR (existing) | High |

### Test Checklist

For each package type, verify:

- [ ] **Installation**: Package installs without errors
- [ ] **Dependencies**: All dependencies are automatically installed
- [ ] **Launch**: Application launches from menu and command line
- [ ] **Display Server**: Works natively on both Wayland and X11
- [ ] **Virtual Mic**: `pactl` creates virtual microphone successfully
- [ ] **Audio Playback**: Sounds play correctly
- [ ] **Global Hotkeys**: Wayland hotkeys work via `swhkd`; X11 hotkeys work via native X11 backend
- [ ] **Mic Passthrough**: Real mic mixes with soundboard audio
- [ ] **File Operations**: Drag-and-drop and folder sync work
- [ ] **Theme Integration**: Respects system dark/light theme
- [ ] **Uninstallation**: Removes cleanly without leftover files

### Automated Testing

```bash
# Run unit tests
cd src
cargo test

# Run clippy
cargo clippy --all-targets --all-features -- -D warnings

# Format check
cargo fmt -- --check
```

---

## Release Checklist

### Pre-Release

- [ ] Update version in `src/Cargo.toml`
- [ ] Update `packaging/debian/changelog`
- [ ] Update `packaging/rpm/linux-soundboard.spec` (Version + %changelog)
- [ ] Update `packaging/aur/PKGBUILD` and `packaging/aur/linux-soundboard-git/PKGBUILD`
- [ ] Regenerate AUR `.SRCINFO` files
- [ ] Update `packaging/flatpak/com.linuxsoundboard.app.metainfo.xml` (add release)
- [ ] Update `README.md` (version numbers in download links)
- [ ] Run all tests: `cargo test`
- [ ] Run clippy: `cargo clippy`
- [ ] Format code: `cargo fmt`

### Build All Packages

```bash
# AppImage
./packaging/linux/package-appimage.sh

# DEB (on Ubuntu/Debian)
./packaging/debian/package-deb.sh

# RPM (on Fedora or with mock)
./packaging/rpm/package-rpm.sh

# Flatpak
./packaging/flatpak/package-flatpak.sh

# Generic release bundle for other distros
./packaging/linux/package-release.sh
```

### Test Packages

- [ ] Test AppImage on Ubuntu 24.04 (Wayland)
- [ ] Test DEB on Ubuntu 24.04
- [ ] Test RPM on Fedora 40
- [ ] Test Flatpak on any distribution
- [ ] Verify all packages on fresh VMs

### Create Release

1. **Tag Release**:
   ```bash
   git tag -a v1.1.0 -m "Release v1.1.0"
   git push origin v1.1.0
   ```

2. **Create GitHub Release**:
   - Go to GitHub Releases
   - Create new release from tag `v1.1.0`
   - Write release notes highlighting full native Wayland and X11 support
   - Upload all packages:
     - `linux-soundboard-x86_64.AppImage`
     - `linux-soundboard-1.1.0-x86_64.AppImage`
     - `linux-soundboard-1.1.0-linux-x86_64.tar.gz`
     - `linux-soundboard_1.1.0-1_amd64.deb`
     - `linux-soundboard-1.1.0-1.fc40.x86_64.rpm`
     - `linux-soundboard-1.1.0.flatpak`

3. **Update AUR**:
   - Update stable and git `PKGBUILD` files
   - Update `.SRCINFO`
   - Push to AUR repository

4. **Submit to Flathub** (if ready):
   - Follow `packaging/flatpak/FLATHUB_SUBMISSION.md`

### Post-Release

- [ ] Announce on GitHub Discussions
- [ ] Update project website (if applicable)
- [ ] Monitor issue tracker for bug reports
- [ ] Update documentation if needed

---

## Troubleshooting

### AppImage Issues

**Problem**: FUSE error when running AppImage
```
fuse: failed to exec fusermount: No such file or directory
```
**Solution**: Install FUSE2
```bash
# Ubuntu/Debian
sudo apt install libfuse2

# Fedora
sudo dnf install fuse-libs
```

**Problem**: `pactl` not found during build
**Solution**: Install PulseAudio utilities on build system
```bash
sudo apt install pulseaudio-utils  # Ubuntu/Debian
sudo dnf install pulseaudio-utils  # Fedora
```

**Problem**: GTK plugin patching fails
**Solution**: Check if `linuxdeploy-plugin-gtk.sh` was downloaded correctly
```bash
ls -la dist/.appimage-tools/linuxdeploy-plugin-gtk.sh
```

---

### DEB Package Issues

**Problem**: `dpkg-buildpackage: command not found`
**Solution**: Install build tools
```bash
sudo apt install dpkg-dev debhelper
```

**Problem**: Missing build dependencies
**Solution**: Install all required development packages
```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libpulse-dev libx11-dev libxi-dev
```

---

### RPM Package Issues

**Problem**: `rpmbuild: command not found`
**Solution**: Install RPM build tools
```bash
sudo dnf install rpm-build
```

**Problem**: Source tarball creation fails
**Solution**: Ensure you're in a git repository or use manual tarball creation

---

### Flatpak Issues

**Problem**: GNOME SDK 47 not found
**Solution**: Install SDK manually
```bash
flatpak install flathub org.gnome.Platform//47 org.gnome.Sdk//47
```

**Problem**: `cargo-sources.json` missing
**Solution**: Generate it manually
```bash
cd packaging/flatpak
python3 flatpak-cargo-generator.py ../../src/Cargo.lock -o cargo-sources.json
```

**Problem**: Build fails with Rust errors
**Solution**: Ensure `Cargo.lock` is up to date
```bash
cd src
cargo update
cargo build --release
```

---

## Version Management

### Semantic Versioning

Linux Soundboard follows [Semantic Versioning](https://semver.org/):
- **MAJOR**: Incompatible API changes
- **MINOR**: New features (backward compatible)
- **PATCH**: Bug fixes (backward compatible)

Current version: **1.1.0**

### Files to Update

When bumping version, update these files:
1. `src/Cargo.toml` - `version = "1.1.0"`
2. `packaging/debian/changelog` - Add new entry
3. `packaging/rpm/linux-soundboard.spec` - `Version:` and `%changelog`
4. `packaging/flatpak/com.linuxsoundboard.app.metainfo.xml` - Add `<release>` entry
5. `README.md` - Update download links if needed

---

## Continuous Integration

GitHub Actions workflows are located in `.github/workflows/`:

- `ci.yml` - Build and test on every push
- `release.yml` - Build all packages on tag push

See Phase 6 of the implementation plan for CI/CD setup.

---

## Support

For packaging questions or issues:
- GitHub Issues: https://github.com/germanua/Linux-SoundBoard/issues
- Discussions: https://github.com/germanua/Linux-SoundBoard/discussions

---

**Last Updated**: 2026-03-24  
**Version**: 1.1.0
