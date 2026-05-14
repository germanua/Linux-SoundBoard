# Installation Guide

## Quick install — one command

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/install.sh | bash
```

`install.sh` detects your distro and installs the right way for your system:

| Distro                       | What happens                                                     |
| ---------------------------- | ---------------------------------------------------------------- |
| Arch / CachyOS / EndeavourOS | Installs `linux-soundboard-git` from the AUR via yay/paru        |
| Debian / Ubuntu              | Downloads and installs the `.deb` package                        |
| Fedora                       | Downloads and installs the `.rpm` package                        |
| Everything else              | Downloads the release tarball and runs `install-user.sh install` |

On Wayland sessions `install.sh` also installs `swhkd` for global hotkeys automatically.

---

## Two scripts, different jobs

| Script            | Who runs it                                                               | What it does                                                                        |
| ----------------- | ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `install.sh`      | You, via the one-liner above                                              | Detects distro, installs via package manager or tarball, handles swhkd on Wayland   |
| `install-user.sh` | Called by `install.sh`, or by you after a manual download or source build | Configures per-user install state: engine service, desktop entry, icons, and legacy audio cleanup |

`install-user.sh` is the low-level tool. `install.sh` is the smart wrapper that calls it when needed and handles the rest (package manager, swhkd, PipeWire services).

---

## Manual install (tarball)

For source builds or when you want to manage the download yourself:

### Step-by-step install

```bash
# 1. Download the latest release tarball from the Releases page
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-2.0.0-linux-x86_64.tar.gz

# 2. Extract it
tar -xzf linux-soundboard-2.0.0-linux-x86_64.tar.gz
cd linux-soundboard-2.0.0-linux-x86_64

# 3. Run the installer — an interactive menu guides you through the install
./install-user.sh
```

Or install non-interactively, skipping the menu:

```bash
./install-user.sh install
```

### What the installer configures

| Item                | Path                                                          | Effect                                              |
| ------------------- | ------------------------------------------------------------- | --------------------------------------------------- |
| Binary              | `~/.local/opt/linux-soundboard/linux-soundboard`              | The main executable                                 |
| Desktop entry       | `~/.local/share/applications/com.linuxsoundboard.app.desktop` | App appears in launcher                             |
| Icons               | `~/.local/share/icons/hicolor/*/apps/linux-soundboard.*`      | Icon set for all sizes                              |
| Engine service      | `~/.config/systemd/user/linux-soundboard-engine.service`      | Starts the audio engine at login                    |
| Legacy cleanup      | Old PipeWire/PulseAudio/WirePlumber soundboard routing files  | Disables obsolete persistent virtual mic setup      |
| Microphone routing  | App setting in `~/.config/linux-soundboard/config.json`       | Routes recording apps while leaving system defaults alone by default |

The engine creates `Linux Soundboard Mic` at runtime while it is running. It uses low PipeWire priority, unmutes the virtual mic on registration, and keeps EasyEffects or your real microphone as the system default unless you explicitly enable **Game compatibility mode**.

### Installer commands

```bash
# Interactive menu (runs automatically when called with no arguments in a terminal)
./install-user.sh

# Install, pointing to a specific binary
./install-user.sh install /path/to/linux-soundboard

# Re-apply system configuration without touching library data
./install-user.sh repair

# Show what is currently installed and service status
./install-user.sh status

# Uninstall with interactive prompt about mic default restoration
./install-user.sh remove

# Uninstall without any prompts, keep library/config data
./install-user.sh remove --yes --keep-data

# Uninstall and restore the microphone that was default before install
./install-user.sh remove --yes --restore-default-source
```

---

## Package managers

### Arch Linux, CachyOS, EndeavourOS

```bash
yay -S linux-soundboard-git
# or
paru -S linux-soundboard-git
```

The AUR package installs the app, icons, helper files, and the user audio-engine service. It does not install a persistent PipeWire virtual mic config.

### Ubuntu and Debian

Download the `.deb` from the [Releases page](https://github.com/germanua/Linux-SoundBoard/releases/latest):

```bash
sudo apt install ./linux-soundboard_2.0.0-1_amd64.deb
```

Required runtime packages (usually already present on modern Ubuntu/Debian):

```
pipewire  wireplumber  libpulse0
```

After a DEB install, run `install-user.sh repair` once without a binary argument to set up the engine service for your account and disable obsolete user-level audio routing files:

```bash
./install-user.sh repair
```

### Fedora

```bash
sudo dnf install ./linux-soundboard-2.0.0-1.x86_64.rpm
```

Required runtime packages:

```
pipewire  wireplumber  pulseaudio-libs
```

Same as Debian: run `./install-user.sh repair` after the RPM install to configure the engine service and clean obsolete user-level audio routing for your account.

---

## AppImage (portable, no install)

The AppImage can run without any installation:

```bash
chmod +x linux-soundboard-x86_64.AppImage
./linux-soundboard-x86_64.AppImage
```

The AppImage creates the virtual mic only while its audio engine is running. It does **not** install the engine service or register a desktop entry by itself. Use `install-user.sh install linux-soundboard-x86_64.AppImage` for a proper installation from the AppImage.

If AppImage reports a FUSE error:

```bash
# Ubuntu / Debian
sudo apt install libfuse2
# Fedora
sudo dnf install fuse-libs
# Arch
sudo pacman -S fuse2
# openSUSE
sudo zypper install fuse
```

---

## Wayland and global hotkeys

On Wayland, Linux Soundboard uses `swhkd` for global hotkeys.

**In-app install:** When the app detects that `swhkd` is missing or inactive, a banner appears at the top of the window with an **Install** button. Clicking it runs a PolicyKit-authorized build and install flow entirely within the app. No terminal required.

Requirements for the in-app install:

- Native install (DEB / RPM / AUR / AppImage on host), not a Flatpak sandbox
- `pkexec` available (provided by `policykit-1` / `polkit`)
- Network access to clone `swhkd` sources from GitHub

**Manual install:**

- Arch family: `yay -S swhkd-bin` or `yay -S swhkd-git`
- Other distros: see [upstream install notes](https://github.com/waycrate/swhkd/blob/main/INSTALL.md)

On **X11 and XWayland**, the app uses a native XInput2 backend. No `swhkd` needed.

---

## Build from source

### Install build dependencies

**Arch:**

```bash
sudo pacman -S cargo rust pkgconf imagemagick gtk4 libadwaita \
  libpulse alsa-lib libx11 libxi pipewire wireplumber
```

**Debian / Ubuntu:**

```bash
sudo apt install build-essential cargo rustc pkg-config imagemagick \
  libgtk-4-dev libadwaita-1-dev libpulse-dev libasound2-dev \
  libx11-dev libxi-dev pipewire wireplumber
```

**Fedora:**

```bash
sudo dnf install cargo rust gcc gcc-c++ clang pkg-config ImageMagick \
  gtk4-devel libadwaita-devel pulseaudio-libs-devel alsa-lib-devel \
  libX11-devel libXi-devel pipewire wireplumber
```

### Build and install

```bash
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard/src
cargo build --release

# Install using the user installer, pointing it at the freshly built binary
cd ..
./packaging/linux/install-user.sh install ./src/target/release/linux-soundboard
```

The installer detects the binary next to the script automatically when run from the repository root.

After rebuilding, run `./packaging/linux/install-user.sh repair` to copy the new binary into place and restart the engine service.

---

## After install: first launch checklist

1. Launch Linux Soundboard from your application menu or run `linux-soundboard` in a terminal.
2. Confirm PipeWire sees the virtual microphone:
   ```bash
   wpctl status -n | grep Soundboard
   ```
3. In Discord, OBS, Zoom, or your target application, select **Linux Soundboard Mic** as the input device when the app exposes a microphone picker.
4. Leave **Microphone Routing → Auto-route while running** enabled for movable apps. For games or recorders that only read the system default mic, switch to **Game compatibility mode** before launching them.
5. Add a sound folder or drag audio files into the library.
6. On Wayland, click **Install** in the hotkey warning banner if global hotkeys are not working.

---

## Troubleshooting

If anything goes wrong after install, see [TROUBLESHOOTING.md](TROUBLESHOOTING.md).

Common quick fixes:

```bash
# Re-run system configuration without reinstalling
./install-user.sh repair

# Manually restart audio services
systemctl --user restart pipewire wireplumber

# Manually restart the engine service
systemctl --user restart linux-soundboard-engine.service

# Check engine service logs
journalctl --user -u linux-soundboard-engine.service -n 50
```

---

## Flatpak

The repository contains Flatpak packaging files, but no Flathub submission is published yet. Flatpak sandboxes also restrict PipeWire and systemd access so `install-user.sh` does not apply inside a Flatpak sandbox.
