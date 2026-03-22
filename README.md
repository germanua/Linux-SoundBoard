<p align="center">
  <img src="FULLLOGO.png" alt="Linux Soundboard" width="400">
</p>

<p align="center">
  <strong>A high-performance, native Linux soundboard built with Rust, GTK4, and Libadwaita</strong>
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
  <a href="https://img.shields.io/badge/platform-Linux-orange?style=flat-square">
    <img src="https://img.shields.io/badge/platform-Linux-orange?style=flat-square" alt="Platform">
  </a>
  <a href="https://img.shields.io/badge/rust-1.85+-red?style=flat-square&logo=rust">
    <img src="https://img.shields.io/badge/rust-1.85+-red?style=flat-square&logo=rust" alt="Rust">
  </a>
  <a href="https://img.shields.io/badge/GTK-4-green?style=flat-square&logo=gnome">
    <img src="https://img.shields.io/badge/GTK-4-green?style=flat-square&logo=gnome" alt="GTK4">
  </a>
  <a href="https://github.com/germanua/Linux-SoundBoard/stargazers">
    <img src="https://img.shields.io/github/stars/germanua/Linux-SoundBoard?style=flat-square&logo=github" alt="Stars">
  </a>
</p>

---

Play sounds directly into your microphone stream for Discord, OBS, Zoom, and any other application. Features a virtual microphone that seamlessly mixes your voice with soundboard audio.

## 📦 Quick Install

| Distribution         | Command                                                                                                                     |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| **Arch Linux (AUR)** | `yay -S linux-soundboard-git`                                                                                               |
| **Ubuntu/Debian**    | [Download AppImage](https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-x86_64.AppImage) |
| **Fedora/openSUSE**  | [Download AppImage](https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-x86_64.AppImage) |
| **Other**            | [Build from source](#-build-from-source)                                                                                    |

---

## ✨ Features

- 🚀 **Native Performance**: Built with Rust and GTK4 for a fast, memory-efficient experience.
- 🎤 **Virtual Microphone**: Automatically creates a virtual audio device to route soundboard audio to other applications.
- 🎙️ **Mic Passthrough**: Mix your real microphone with the soundboard audio so your friends hear both.
- 🔊 **Advanced Audio Processing**:
  - **LUFS Normalization (Auto-Gain)**: Keep all your sounds at the same volume level automatically.
  - **Static & Dynamic Modes**: Choose between pre-scanned normalization or real-time dynamic lookahead.
  - **Independent Volume Control**: Separate sliders for your local speakers and the virtual microphone.
- ⌨️ **Global Hotkeys**: Bind sounds and controls (Play/Pause, Stop All, Next/Prev) through the app's X11 hotkey backend.
- 📁 **Organized Library**:
  - **Sound Tabs**: Categorize your sounds into custom tabs.
  - **Folder Sync**: Auto-scan directories for new audio files.
  - **Drag & Drop**: Easily import sounds by dropping files into the window.
- 🎨 **Modern UI**: Follows the Libadwaita design language with support for system dark/light themes.
- 📊 **Diagnostics**: Built-in memory monitoring and audio status tracking.

---

## 📥 Installation

### <img src="https://www.archlinux.org/static/logos/archlinux-logo-dark-90dpi.ebdee92a15b3.png" height="20"> Arch Linux (AUR)

The easiest way to install on Arch-based distributions (Arch, Manjaro, EndeavourOS, etc.):

```bash
# Using yay
yay -S linux-soundboard-git

# Using paru
paru -S linux-soundboard-git

# Manual AUR installation
git clone https://aur.archlinux.org/linux-soundboard-git.git
cd linux-soundboard-git
makepkg -si
```

The `-git` package automatically pulls the latest version from GitHub.

---

### <img src="https://assets.ubuntu.com/v1/29985a98-ubuntu-logo32.png" height="20"> Ubuntu / Debian

**Recommended: AppImage** (Ubuntu 24.04+, Debian 13+)

```bash
# Download the AppImage
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-x86_64.AppImage

# Make it executable
chmod +x linux-soundboard-x86_64.AppImage

# Run it
./linux-soundboard-x86_64.AppImage
```

**Install required audio dependencies:**

```bash
sudo apt update
sudo apt install pulseaudio-utils pipewire pipewire-pulse wireplumber
```

> **Note:** If you get a FUSE error, install `libfuse2` or `libfuse2t64` and try again.

---

### <img src="https://upload.wikimedia.org/wikipedia/commons/thumb/3/3f/Fedora_logo.svg/32px-Fedora_logo.svg.png" height="20"> Fedora

**Option 1: AppImage**

```bash
# Download the AppImage
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-x86_64.AppImage
chmod +x linux-soundboard-x86_64.AppImage
./linux-soundboard-x86_64.AppImage
```

**Option 2: Build from source**

```bash
# Install dependencies
sudo dnf install gcc pkg-config gtk4-devel libadwaita-devel pulseaudio-libs-devel \
                 pipewire pipewire-pulseaudio wireplumber

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Clone and build
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard/src
cargo build --release

# Run
./target/release/linux-soundboard
```

---

### <img src="https://en.opensuse.org/images/4/44/Button-colour.png" height="20"> openSUSE

**Option 1: AppImage**

```bash
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-x86_64.AppImage
chmod +x linux-soundboard-x86_64.AppImage
./linux-soundboard-x86_64.AppImage
```

**Option 2: Build from source**

```bash
# Install dependencies
sudo zypper install gcc pkg-config gtk4-devel libadwaita-devel libpulse-devel \
                    pipewire pipewire-pulseaudio wireplumber

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Clone and build
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard/src
cargo build --release
```

---

### 🔧 Build from Source

<details>
<summary><strong>General build instructions for any distribution</strong></summary>

#### Requirements

- **Rust 1.85+** (via [rustup](https://rustup.rs/))
- **GTK4** and **Libadwaita** development libraries
- **PulseAudio** development libraries
- **PipeWire** with PulseAudio compatibility layer (recommended)

#### Build steps

```bash
# 1. Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 2. Clone the repository
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard

# 3. Build the application
cd src
cargo build --release

# 4. Run
./target/release/linux-soundboard
```

#### Package names by distribution

| Distribution | Packages |
|-------------|----------|
| **Arch** | `gtk4 libadwaita libpulse pipewire pipewire-pulse wireplumber` |
| **Ubuntu/Debian** | `libgtk-4-dev libadwaita-1-dev libpulse-dev pipewire pipewire-pulse wireplumber` |
| **Fedora** | `gtk4-devel libadwaita-devel pulseaudio-libs-devel pipewire pipewire-pulseaudio wireplumber` |
| **openSUSE** | `gtk4-devel libadwaita-devel libpulse-devel pipewire pipewire-pulseaudio wireplumber` |

</details>

---

### 📦 Maintainers: Build Packages

<details>
<summary><strong>Build AppImage</strong></summary>

```bash
./packaging/linux/package-appimage.sh
```

Artifacts are written to `dist/`:
- `dist/linux-soundboard-x86_64.AppImage`
- `dist/linux-soundboard-<version>-x86_64.AppImage`

</details>

<details>
<summary><strong>Build Tarball</strong></summary>

```bash
./packaging/linux/package-release.sh
```

Users install via:
```bash
./install-user.sh
```

</details>

## 🚀 Usage

1. **Launch the App** — On first run, it will initialize the virtual microphone automatically.

2. **Add Sounds**
   - Go to **Settings** → **Add Folder** to scan a directory
   - Or **drag & drop** files directly into a tab

3. **Configure Virtual Mic**
   - In **Discord/OBS/Zoom**, select `Linux_Soundboard_Mic` as your input device
   - Toggle **Mic Passthrough** in the app to mix your voice with sounds

4. **Set Hotkeys**
   - Click the edit icon on a sound to assign a global hotkey
   - Full numpad support: `NumpadAdd`, `NumpadSubtract`, `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`, `NumpadEnter`
   - Configure control hotkeys (Stop All, Play/Pause, Next/Prev) in Settings

---

## 🏗️ Architecture

| Component | Technology |
|-----------|-----------|
| **UI Framework** | GTK4 + Libadwaita (Native Rust bindings) |
| **Audio Engine** | Rodio + Symphonia (MP3, WAV, OGG, FLAC, AAC) |
| **Audio Routing** | PulseAudio/PipeWire via `pactl` |
| **Global Hotkeys** | X11/XInput2 + XKB (X11 and XWayland) |
| **Configuration** | JSON at `~/.config/linux-soundboard/` |

---

## ⚠️ Known Limitations

- Global hotkeys require **X11 or XWayland** — native Wayland not yet supported
- AppImage requires **FUSE** (install `libfuse2` if needed)
- Ubuntu 22.04 / Debian 12: GTK4/Libadwaita too old for source builds — use AppImage

---

## 📄 License

This project is licensed under the **PolyForm Noncommercial 1.0.0** license.

You may use, modify, and share the software for **noncommercial purposes** only. Commercial use requires a separate license from the copyright holder.

See the [LICENSE](LICENSE) file for full terms.

---

<p align="center">
  <sub>Made with ❤️ for the Linux community</sub>
</p>
