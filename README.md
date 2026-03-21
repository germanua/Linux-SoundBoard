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

- **Linux** with **PipeWire** (or PulseAudio)
- **libadwaita** and **gtk4** libraries
- **pactl** (for virtual mic management)

## 📥 Installation

### From Source

Ensure you have the Rust toolchain and development headers for GTK4/Libadwaita installed.

```bash
# Ubuntu/Debian
sudo apt install libgtk-4-dev libadwaita-1-dev libpulse-dev libpipewire-0.3-dev

# Fedora
sudo dnf install gtk4-devel libadwaita-devel libpulse-devel pipewire-devel

# Arch Linux
sudo pacman -S gtk4 libadwaita libpulse pipewire

# Build the project
git clone https://github.com/germanua/linux-soundboard.git
cd linux-soundboard/src-tauri
cargo build --release
```

The executable will be located at `src-tauri/target/release/linux-soundboard`.

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

## 🤝 Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## 📄 License

This project is licensed under the PolyForm Noncommercial 1.0.0 license.

That means people may use, modify, and share the software only for noncommercial purposes under the license terms. Commercial use requires a separate license from the copyright holder.

See the [LICENSE](LICENSE) file for the full terms.
