<p align="center">
  <img src="assets/logo.png" alt="Linux Soundboard" width="320">
</p>

<h1 align="center">Linux Soundboard</h1>

<p align="center">
  Native Linux soundboard with PipeWire virtual microphone, LUFS normalization, and global hotkeys for Wayland and X11.
</p>

<p align="center">
  <a href="https://github.com/germanua/Linux-SoundBoard/releases/latest">
    <img src="https://img.shields.io/github/v/release/germanua/Linux-SoundBoard?style=for-the-badge&logo=github" alt="Latest Release">
  </a>
  <a href="https://aur.archlinux.org/packages/linux-soundboard-git">
    <img src="https://img.shields.io/aur/version/linux-soundboard-git?style=for-the-badge&logo=archlinux&color=1793d1" alt="AUR">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-PolyForm%20NC%201.0.0-3c8d40?style=for-the-badge" alt="License">
  </a>
</p>

<p align="center">
  <a href="https://github.com/germanua/Linux-SoundBoard/releases/latest"><strong>Download</strong></a>
  ·
  <a href="docs/INSTALL.md"><strong>Install Guide</strong></a>
  ·
  <a href="docs/FEATURE_REFERENCE.md"><strong>Feature Reference</strong></a>
  ·
  <a href="docs/SCREENSHOTS.md"><strong>Screenshots</strong></a>
  ·
  <a href="docs/TROUBLESHOOTING.md"><strong>Troubleshooting</strong></a>
</p>

<p align="center"><b>Install with one command — no root required:</b></p>

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/install.sh | bash
```

<p align="center">Sets up the virtual mic, engine service, desktop entry, and icons automatically.</p>

---

## What it does

Linux Soundboard routes sound effects into a **permanent virtual microphone** (`Linux Soundboard Mic`) that any app — Discord, OBS, Zoom, Steam games — can select as its input device. Your real microphone stays available for mic passthrough so your voice and soundboard audio share the same virtual input.

Unlike browser-based or Electron wrappers, this is a native Rust + GTK4 + PipeWire application. The audio engine runs as a background systemd user service and keeps the virtual microphone registered even when the UI is closed.

---

## Screenshots

<p align="center">
  <img src="assets/screenshots/Main_dark.png" alt="Main window — dark mode" width="880">
</p>

<p align="center">
  <img src="assets/screenshots/Main_light.png" alt="Main window — light mode" width="880">
</p>

<p align="center">
  <img src="assets/screenshots/Settings_dark1.png" alt="Settings — dark mode" width="420">
  <img src="assets/screenshots/Settings_hotkeys_dark.png" alt="Hotkey settings — dark mode" width="420">
</p>

Full gallery → [docs/SCREENSHOTS.md](docs/SCREENSHOTS.md)

---

## Install

### One-liner (recommended)

The command at the top of this page is all you need.

`install.sh` detects your distro and does the right thing:

| Distro                       | What happens                                                    |
| ---------------------------- | --------------------------------------------------------------- |
| Arch / CachyOS / EndeavourOS | Installs from the AUR via yay or paru                           |
| Debian / Ubuntu              | Downloads and installs the `.deb` package                       |
| Fedora                       | Downloads and installs the `.rpm` package                       |
| Everything else              | Downloads the release tarball and runs the user-space installer |

On Wayland, `swhkd` for global hotkeys is installed automatically.

After install, the per-user setup tool `install-user.sh` handles repair and uninstall:

```bash
install-user.sh repair          # re-register virtual mic, restart engine
install-user.sh remove          # uninstall with interactive prompt
install-user.sh remove --yes    # uninstall without prompts
install-user.sh status          # show what is installed and service status
```

### AppImage (portable, no install)

```bash
chmod +x linux-soundboard-x86_64.AppImage
./linux-soundboard-x86_64.AppImage
```

The AppImage writes the PipeWire config on first launch but does not install the engine service or desktop entry. Run `install-user.sh install linux-soundboard-x86_64.AppImage` for a full install from the AppImage.

---

## Quick Start

1. Install using the method above. The virtual microphone `Linux Soundboard Mic` is now permanently registered with PipeWire.
2. Launch `linux-soundboard` from your application menu or terminal.
3. **In Discord, OBS, Zoom, or your game** — select `Linux Soundboard Mic` as the microphone input. You only need to do this once.
4. For games that read the system default mic (e.g. Arma Reforger): set **Default Microphone** to `Auto While Running` in Settings. The app sets `Linux Soundboard Mic` as default while running and restores your real mic when you exit.
5. Add a sound folder or drag audio files into the window to build your library.
6. On Wayland: if you see a hotkey warning, click **Install** in the banner to install `swhkd` for global hotkeys.

---

## How it works

```
┌─────────────────────────────────────────────────────┐
│  GTK4 UI process                                    │
│  - Sound library, tabs, search                      │
│  - Transport bar (play / pause / stop / scrub)      │
│  - Settings, hotkey management                      │
│  - Communicates via Unix socket IPC                 │
└──────────────────────────┬──────────────────────────┘
                           │ IPC (Unix socket)
┌──────────────────────────▼──────────────────────────┐
│  Audio engine  (linux-soundboard-engine.service)    │
│  - PipeWire streams: local speakers + virtual mic   │
│  - Mic passthrough mixing                           │
│  - Volume, LUFS normalization, seeking, looping     │
│  - Stays running when the UI window is closed       │
└─────────────────────────────────────────────────────┘
```

The engine is a separate systemd user service. This design keeps the virtual microphone and mic passthrough active regardless of whether the UI is open, and means opening/closing the UI window has no audio interruption. Closing the UI window sends a **StopAll** command to the engine first so active sounds stop cleanly.

---

## Feature Highlights

### Playback

- **LUFS normalization** — per-sound gain so every clip plays at a consistent loudness regardless of how it was recorded
- **Three play modes** — Default (play once), Loop (repeat indefinitely), Continue (auto-advance to the next sound)
- **Transport controls** — play/pause, stop all, previous/next, scrub bar with seek
- **Multiple output levels** — independent sliders for local speakers and virtual microphone

### Audio routing

- **Permanent virtual mic** — `Linux Soundboard Mic` is always registered with PipeWire, even when the app is not running
- **Mic passthrough** — blends your real microphone into the virtual mic stream so your voice and sound effects go out on the same input device
- **Default mic takeover** — `Auto While Running` mode sets the virtual mic as the system default while the app is open and restores your real mic when you close it; games and apps that cannot switch inputs still hear you
- **PulseAudio fallback** — works on systems without WirePlumber using a `default.pa` fragment

### Library

- **Tabs** — organize sounds into named tabs; the General tab shows all sounds
- **Folder sync** — point a tab at a folder; files added or removed from disk are reflected automatically
- **Drag and drop** — drag files or folders from a file manager directly into the window
- **Search** — real-time search bar filters the visible list
- **Hotkeys** — assign a global key combination to any sound; Wayland uses `swhkd`, X11 uses a native XInput2 backend

### Global hotkeys

| Session  | Backend        | Notes                                  |
| -------- | -------------- | -------------------------------------- |
| Wayland  | `swhkd`        | In-app one-click install via PolicyKit |
| X11      | Native XInput2 | No extra software needed               |
| XWayland | Native XInput2 | Works without `swhkd`                  |

`~` pass-through prefix is applied automatically so hotkey combinations do not get consumed by the system.

---

## Documentation

| Document                                         | Contents                                                                               |
| ------------------------------------------------ | -------------------------------------------------------------------------------------- |
| [Installation Guide](docs/INSTALL.md)            | Full install, repair, and uninstall instructions including `install-user.sh` reference |
| [Feature Reference](docs/FEATURE_REFERENCE.md)   | Every UI element, right-click menu, control hotkey, and setting                        |
| [Screenshot Gallery](docs/SCREENSHOTS.md)        | Interface screenshots                                                                  |
| [Troubleshooting Guide](docs/TROUBLESHOOTING.md) | PipeWire, renderer, hotkey, and packaging issues                                       |
| [Bug Reporting Guide](docs/BUG_REPORTS.md)       | How to file a useful bug report                                                        |
| [Changelog](docs/CHANGELOG.md)                   | Version history                                                                        |

---

## Build From Source

```bash
# Arch
sudo pacman -S cargo rust pkgconf imagemagick gtk4 libadwaita libpulse alsa-lib libx11 libxi pipewire wireplumber

# Debian / Ubuntu
sudo apt install build-essential cargo rustc pkg-config imagemagick \
  libgtk-4-dev libadwaita-1-dev libpulse-dev libasound2-dev libx11-dev libxi-dev pipewire wireplumber

# Fedora
sudo dnf install cargo rust gcc gcc-c++ clang pkg-config ImageMagick \
  gtk4-devel libadwaita-devel pulseaudio-libs-devel alsa-lib-devel libX11-devel libXi-devel pipewire wireplumber
```

```bash
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard/src
cargo build --release
./packaging/linux/install-user.sh install ./target/release/linux-soundboard
```

Full source-build notes are in [docs/INSTALL.md](docs/INSTALL.md).

---

## Support

- **Issues:** https://github.com/germanua/Linux-SoundBoard/issues
- **Discussions:** https://github.com/germanua/Linux-SoundBoard/discussions
- **AUR package:** https://aur.archlinux.org/packages/linux-soundboard-git

---

## License

Linux Soundboard is licensed under the [PolyForm Noncommercial 1.0.0](LICENSE) license. Commercial use requires a separate license.

---

## Acknowledgments

Linux Soundboard is built on Rust, GTK4, Libadwaita, and the Linux audio ecosystem. Key components include Symphonia for audio decoding, PipeWire and WirePlumber for virtual mic routing, and `swhkd` for Wayland hotkey capture.

Full third-party license notices are in [THIRDPARTY_LICENSES.md](THIRDPARTY_LICENSES.md).
