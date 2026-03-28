<p align="center">
  <img src="assets/logo.png" alt="Linux Soundboard" width="400">
</p>

<h1 align="center">Linux Soundboard</h1>

<p align="center">
  <strong>High-performance soundboard for Linux with virtual microphone support</strong>
</p>

<p align="center">
  <a href="https://github.com/germanua/Linux-SoundBoard/releases/latest">
    <img src="https://img.shields.io/github/v/release/germanua/Linux-SoundBoard?style=for-the-badge&logo=github&color=blue" alt="Latest Release">
  </a>
  <a href="https://aur.archlinux.org/packages/linux-soundboard-git">
    <img src="https://img.shields.io/aur/version/linux-soundboard-git?style=for-the-badge&logo=archlinux&color=1793d1" alt="AUR Version">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-PolyForm%20NC%201.0.0-green?style=for-the-badge" alt="License">
  </a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Linux-orange?style=flat-square" alt="Platform">
  <img src="https://img.shields.io/badge/rust-1.85+-red?style=flat-square&logo=rust" alt="Rust">
  <img src="https://img.shields.io/badge/GTK-4-green?style=flat-square&logo=gnome" alt="GTK4">
  <img src="https://img.shields.io/github/stars/germanua/Linux-SoundBoard?style=flat-square&logo=github" alt="Stars">
</p>

---

## 🎵 What is Linux Soundboard?

Play sounds directly into your microphone stream for **Discord**, **OBS**, **Zoom**, and any other application. Features a virtual microphone that seamlessly mixes your voice with soundboard audio.

Built with **Rust**, **GTK4**, and **Libadwaita** for native performance and modern Linux desktop integration.

### ✨ Key Features

- 🎤 **Virtual Microphone** - Automatically creates a virtual audio device
- 🎙️ **Mic Passthrough** - Mix your real microphone with soundboard audio
- 🔊 **LUFS Normalization** - Keep all sounds at consistent volume
- ⌨️ **Global Hotkeys** - Native Wayland hotkeys via swhkd, plus full native X11 support
- 🎨 **Modern UI** - Native GTK4/Libadwaita with dark mode support
- 📁 **Organized Library** - Tabs, folder sync, drag & drop
- 🖥️ **Display Server Support** - Full native Wayland and X11 support
- 📦 **Multiple Formats** - AppImage, DEB, RPM, Flatpak, AUR

---

## 🚀 Quick Start

### Choose Your Installation Method

| Distribution | Recommended | Command |
|--------------|-------------|---------|
| **Arch Linux** | AUR | `yay -S linux-soundboard-git` |
| **Ubuntu/Debian** | DEB Package | [Download .deb](https://github.com/germanua/Linux-SoundBoard/releases/latest) |
| **Fedora/RHEL** | RPM Package | [Download .rpm](https://github.com/germanua/Linux-SoundBoard/releases/latest) |
| **Any Linux** | Flatpak | [Download .flatpak](https://github.com/germanua/Linux-SoundBoard/releases/latest) |
| **Portable** | AppImage | [Download .AppImage](https://github.com/germanua/Linux-SoundBoard/releases/latest) |

### 3-Step Setup

1. **Install** using your preferred method above
2. **Launch** the application from your menu or terminal
3. **Select** `Linux_Soundboard_Mic` as input in Discord/OBS/Zoom

That's it! 🎉

---

## 📦 Installation

### Arch Linux (AUR)

```bash
yay -S linux-soundboard-git
```

Or with paru:
```bash
paru -S linux-soundboard-git
```

---

### Ubuntu / Debian

**Option 1: DEB Package (Recommended)**

```bash
# Download
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard_1.1.0-1_amd64.deb

# Install
sudo apt install ./linux-soundboard_1.1.0-1_amd64.deb

# Run
linux-soundboard
```

**Option 2: AppImage**

```bash
# Download
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-x86_64.AppImage

# Make executable
chmod +x linux-soundboard-x86_64.AppImage

# Run
./linux-soundboard-x86_64.AppImage
```

> **Note:** If you get a FUSE error, install `libfuse2`: `sudo apt install libfuse2`

---

### Fedora / RHEL

**Option 1: RPM Package (Recommended)**

```bash
# Download
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-1.1.0-1.fc40.x86_64.rpm

# Install
sudo dnf install ./linux-soundboard-1.1.0-1.fc40.x86_64.rpm

# Run
linux-soundboard
```

**Option 2: AppImage**

```bash
# Download
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-x86_64.AppImage

# Make executable and run
chmod +x linux-soundboard-x86_64.AppImage
./linux-soundboard-x86_64.AppImage
```

> **Note:** If you get a FUSE error, install `fuse-libs`: `sudo dnf install fuse-libs`

---

### Flatpak (Universal)

**Coming soon to Flathub!**

For now, download the bundle:

```bash
# Download
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-1.1.0.flatpak

# Install
flatpak install linux-soundboard-1.1.0.flatpak

# Run
flatpak run com.linuxsoundboard.app
```

---

### Build from Source

<details>
<summary><strong>Click to expand build instructions</strong></summary>

#### Requirements

- Rust 1.85+
- GTK4 and Libadwaita development libraries
- PulseAudio development libraries
- PipeWire + WirePlumber (recommended)

#### Install Dependencies

**Ubuntu/Debian:**
```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libpulse-dev \
  libx11-dev libxi-dev pkg-config imagemagick cargo rustc
```

**Fedora:**
```bash
sudo dnf install gtk4-devel libadwaita-devel pulseaudio-libs-devel \
  libX11-devel libXi-devel pkg-config ImageMagick cargo rust alsa-lib-devel
```

**Arch Linux:**
```bash
sudo pacman -S gtk4 libadwaita libpulse libx11 libxi pkgconf imagemagick cargo
```

#### Build

```bash
# Clone repository
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard

# Build
cd src
cargo build --release

# Run
./target/release/linux-soundboard
```

</details>

---

## 🎮 Usage

### Basic Setup

1. **Launch** Linux Soundboard
2. **Add sounds**:
   - Click **Settings** → **Add Folder** to scan a directory
   - Or **drag & drop** audio files into the window
3. **Configure virtual mic**:
   - In Discord/OBS/Zoom, select `Linux_Soundboard_Mic` as input
   - Toggle **Mic Passthrough** to mix your voice with sounds

### Global Hotkeys

1. Click the **edit icon** on any sound
2. Press your desired key combination
3. Hotkeys work system-wide (even when app is minimized)

**Supported keys:** All standard keys plus full numpad support

### Audio Normalization

Enable **LUFS Normalization** in settings to keep all sounds at consistent volume:
- **Static Mode**: Pre-scans all sounds (faster playback)
- **Dynamic Mode**: Real-time normalization (more accurate)

---

## 🏗️ Architecture

| Component | Technology |
|-----------|------------|
| **Language** | Rust 1.85+ |
| **UI Framework** | GTK4 + Libadwaita |
| **Audio Engine** | Rodio + Symphonia |
| **Audio Routing** | PulseAudio + PipeWire |
| **Global Hotkeys** | swhkd on Wayland + native X11/XInput2 backend |
| **Formats** | MP3, WAV, OGG, FLAC, AAC |

### Display Server Support

- ✅ **Wayland**: Native GTK Wayland support with swhkd hotkeys
- ✅ **X11**: Native X11 backend with XInput2 hotkeys
- ✅ **TTY**: Full support via swhkd
- ✅ **XWayland**: Supported when you want the X11 backend inside a Wayland session

---

## 🐛 Troubleshooting

### Common Issues

**Virtual microphone not created?**
```bash
# Install PipeWire
sudo apt install pipewire pipewire-pulse wireplumber  # Ubuntu/Debian
sudo dnf install pipewire pipewire-pulseaudio wireplumber  # Fedora

# Enable and start
systemctl --user enable --now pipewire pipewire-pulse wireplumber
```

**AppImage won't run?**
```bash
# Install FUSE2
sudo apt install libfuse2  # Ubuntu/Debian
sudo dnf install fuse-libs  # Fedora
```

**Hotkeys not working?**
```bash
# Wayland sessions use swhkd for global hotkeys
pgrep swhkd

# Arch users can install an AUR package:
# yay -S swhkd-bin
#
# Debian/Ubuntu/Fedora users need the upstream install guide:
# https://github.com/waycrate/swhkd/blob/main/INSTALL.md
#
# Check setuid permissions:
ls -l "$(command -v swhkd)"  # Should show 'rws' permissions

# Manual fix if needed:
sudo chmod u+s "$(command -v swhkd)"

# X11 sessions can also use the native X11 backend directly
```

**More issues?** Check the [**Troubleshooting Guide**](docs/TROUBLESHOOTING.md)

---

## 📚 Documentation

- [**Troubleshooting Guide**](docs/TROUBLESHOOTING.md) - Solutions to common issues
- [**Changelog**](docs/CHANGELOG.md) - Version history and changes
- [**Contributing Guide**](docs/CONTRIBUTING.md) - How to contribute
- [**Packaging Guide**](PACKAGING.md) - Build packages for distributions

---

## 🆕 What's New in v1.1.0

- ✅ **Full Native Wayland + X11 Support** - Wayland via swhkd, X11 via the native backend
- ✅ **DEB & RPM Packages** - Native packages for Ubuntu, Debian, Fedora
- ✅ **Flatpak Support** - Universal package for all distributions
- ✅ **Improved AppImage** - Bundled dependencies, better compatibility
- ✅ **Better Error Messages** - Know exactly what's missing
- ✅ **Automated Builds** - CI/CD for all package types

[**Full Changelog**](docs/CHANGELOG.md)

---

## 🤝 Contributing

Contributions are welcome! Please read the [Contributing Guide](docs/CONTRIBUTING.md) first.

### Ways to Contribute

- 🐛 Report bugs
- 💡 Suggest features
- 📝 Improve documentation
- 🔧 Submit pull requests
- ⭐ Star the project

---

## 📄 License

This project is licensed under the **PolyForm Noncommercial 1.0.0** license.

- ✅ **Free for personal use**
- ✅ **Free for educational use**
- ✅ **Free for non-profit use**
- ❌ **Commercial use requires separate license**

See the [LICENSE](LICENSE) file for full terms.

---

## 🌟 Support

- **Issues**: [GitHub Issues](https://github.com/germanua/Linux-SoundBoard/issues)
- **Discussions**: [GitHub Discussions](https://github.com/germanua/Linux-SoundBoard/discussions)
- **AUR**: [linux-soundboard-git](https://aur.archlinux.org/packages/linux-soundboard-git)

---

## 🙏 Acknowledgments

Built with:
- [Rust](https://www.rust-lang.org/) - Systems programming language
- [GTK4](https://www.gtk.org/) - UI toolkit
- [Libadwaita](https://gnome.pages.gitlab.gnome.org/libadwaita/) - GNOME design patterns
- [Rodio](https://github.com/RustAudio/rodio) - Audio playback
- [PipeWire](https://pipewire.org/) - Audio routing

---

<p align="center">
  <sub>Made with ❤️ for the Linux community</sub>
</p>

<p align="center">
  <a href="https://github.com/germanua/Linux-SoundBoard/stargazers">⭐ Star this project</a>
  •
  <a href="https://github.com/germanua/Linux-SoundBoard/issues">🐛 Report Bug</a>
  •
  <a href="https://github.com/germanua/Linux-SoundBoard/discussions">💬 Discussions</a>
</p>
