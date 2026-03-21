# Linux Soundboard

A high-performance, native Linux soundboard application built with **Rust**, **GTK4**, and **Libadwaita**. Designed for seamless integration with **PipeWire** and **PulseAudio**, it allows you to play sounds directly into your microphone stream for apps like Discord, OBS, and Zoom.

![License](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux-orange.svg)
![Language](https://img.shields.io/badge/language-Rust-red.svg)

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

## 🛠️ Requirements

- **Runtime**
  - GTK4 + Libadwaita
  - PulseAudio client libraries
  - `pactl` (used to create the virtual sink/source)
  - PipeWire recommended: `pipewire` + `pipewire-pulse` + `wireplumber`
- **Build from source**
  - Rust **1.85+** via `rustup`
  - GCC/Clang toolchain + `pkg-config`
  - GTK4 / Libadwaita / GLib / PulseAudio development packages

## ✅ Ubuntu/Debian Support

- **Recommended for source builds:** Ubuntu **24.04+** or Debian **13+**
- **Not recommended for source builds:** Ubuntu 22.04 / Debian 12, because their GTK4 / Libadwaita packages are too old for the current UI API usage
- **Do not use** Ubuntu/Debian's `apt install cargo rustc` toolchain for this repo; use `rustup`

## 📥 Installation

### Ubuntu/Debian: Run a Prebuilt Binary

If you distribute a release archive produced by `packaging/linux/package-release.sh`, users should install the runtime packages first:

```bash
sudo apt update
sudo apt install \
  libgtk-4-1 \
  libadwaita-1-0 \
  libpulse0 \
  pulseaudio-utils \
  pipewire \
  pipewire-pulse \
  wireplumber
```

Then extract the archive and run:

```bash
./install-user.sh
```

That installs the binary into `~/.local/opt/linux-soundboard/` and creates a desktop launcher.

### Ubuntu/Debian: Build From Source

Install Rust with `rustup` first:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup update stable
cargo --version
```

If you use Ubuntu's `apt install cargo rustc`, you may hit this:

```text
lock file version 4 requires `-Znext-lockfile-bump`
```

That means the distro Rust toolchain is too old. Use the `rustup` toolchain above instead.

```bash
sudo apt update
sudo apt install \
  build-essential \
  pkg-config \
  curl \
  libglib2.0-dev \
  libgtk-4-dev \
  libadwaita-1-dev \
  libpulse-dev \
  pulseaudio-utils \
  pipewire \
  pipewire-pulse \
  wireplumber

git clone https://github.com/germanua/linux-soundboard.git
cd linux-soundboard/src-tauri
cargo build --release
```

The executable will be located at `src-tauri/target/release/linux-soundboard`.

### Notes

- `libpipewire-0.3-dev` is **not** required for the current codebase.
- Global hotkeys use an **X11 backend** and may only work under X11 or XWayland.
- For best binary compatibility, build release artifacts on the **oldest distro you want to support**. A binary built on a newer distro can fail on older Ubuntu/Debian releases because of newer `glibc` requirements.

## 🚀 Usage

1. **Launch the App**: On first run, it will attempt to initialize the virtual microphone.
2. **Add Sounds**:
   - Go to **Settings** → **Add Folder** to scan a directory.
   - Or **Drag & Drop** files directly into a tab.
3. **Configure Virtual Mic**:
   - In **Discord/OBS**, select `Linux_Soundboard_Mic` as your input device.
   - Toggle **Mic Passthrough** in the app settings to include your voice.
4. **Set Hotkeys**:
   - Click the edit icon on a sound to assign a global hotkey.
   - Configure control hotkeys (Stop All, etc.) in the Settings panel.

## 🏗️ Architecture

- **UI**: GTK4 + Libadwaita (Native Rust bindings)
- **Audio Engine**: Rodio + Symphonia (Support for MP3, WAV, OGG, FLAC, AAC)
- **Audio Routing**: PulseAudio/PipeWire via `pactl` module loading
- **Global Hotkeys**: X11/XKB backend (may work under XWayland)
- **Config**: JSON-based storage at `~/.config/linux-soundboard/`

## 📄 License

This project is licensed under the PolyForm Noncommercial 1.0.0 license.

That means people may use, modify, and share the software only for noncommercial purposes under the license terms. Commercial use requires a separate license from the copyright holder.

See the [LICENSE](LICENSE) file for the full terms.
