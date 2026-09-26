# Installation Guide

## Release format

**Latest public release: 2.4.7.** The project is transitioning new releases to a single x86_64 AppImage distribution path. Arch/AUR, Debian `.deb`, RPM, and binary-tarball artifacts from 2.4.4 and earlier remain historical releases.

The AppImage contains the application plus the per-user installer used to configure the persistent audio engine, desktop entry, icons, and virtual-microphone integration.

### AppImage compatibility

The AppImage being prepared for the next public release is built for **x86_64** and requires **glibc 2.39 or newer**. Tested compatible distro baselines include Ubuntu 24.04/26.04, Debian 13, Fedora 40+, current Arch-family distributions, and openSUSE Leap 16/Tumbleweed. Ubuntu 22.04, Debian 12, and openSUSE Leap 15.6 have older glibc versions and are not compatible with the prepared AppImage.

The prepared authenticated installer and AppImage runtime preflight both check this ABI requirement before the application is launched.

## Release download verification

`bootstrap-install.sh` first verifies the release-published `install.sh` through `SHA256SUMS.txt.minisig` and the pinned [`release.pub`](../release.pub) key. The authenticated `install.sh` then verifies the AppImage through the same signed manifest. If Minisign is not installed, both scripts use the pinned verifier bootstrap. Missing assets, invalid signatures, missing manifest entries, and checksum mismatches stop the install.

To verify a downloaded AppImage before running it:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh \
  | bash -s -- verify ./linux-soundboard-2.4.7-x86_64.AppImage
```

For an older release, name its tag with `--version vX.Y.Z`.

## Quick install — one command

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh | bash
```

For current releases, **Automatic** means AppImage. The installer:

1. reads the latest GitHub release,
2. verifies the signed release installer and executes only that authenticated copy,
3. verifies the signed checksum manifest and downloads the x86_64 AppImage after verifying the host ABI,
4. checks for an older native package that would shadow the user installation,
5. extracts the bundled installer and installs the AppImage under `~/.local/opt/linux-soundboard/`, and
6. configures the user audio engine and, on Wayland, the hotkey helper.

The AppImage itself stays under your user account. On Wayland, the one-command installer copies the already authenticated release into a root-owned temporary location, verifies that copy against the signed release checksum, and provisions the fixed helper under `/usr/libexec/linux-soundboard`; this step may ask for `sudo`. Removing a legacy native package also requires the system package manager.

### Existing AUR / DEB / RPM installations

Native packages from 2.4.4 and earlier may still be present under `/usr/bin`. That binary takes precedence over the user-installed AppImage and can cause the GUI and persistent engine to run different versions. `install.sh` detects this situation and offers to remove the old package before installing the prepared AppImage.

The repository retains legacy AUR/DEB/RPM packaging files for maintenance and migration support, but they are **not planned artifacts for the next public release** unless that policy changes before publication.

### AppImage from the Releases page

```bash
chmod +x linux-soundboard-2.4.7-x86_64.AppImage
./linux-soundboard-2.4.7-x86_64.AppImage
```

On first direct launch:

- **Install for persistent virtual mic** copies the AppImage to `~/.local/opt/linux-soundboard/linux-soundboard`, registers the desktop entry and user service, and connects the GUI to the matching service engine.
- **Run temporarily** keeps everything scoped to that launch and restores eligible routing state during shutdown.
- **Exit** changes nothing.

Once installed, opening a newer downloaded AppImage updates the installed copy and restarts the matching user engine. The version marker prevents an older downloaded AppImage from silently downgrading a newer installation.

### Command-line operations

```bash
./install.sh install                    # current release; AppImage
./install.sh install --method appimage  # explicit AppImage path
./install.sh install --version vX.Y.Z   # install a published AppImage release
./install.sh versions                   # list published releases
./install.sh verify ./DOWNLOADED_FILE   # verify a release download
./install.sh fix                        # guided repair
./install.sh report --output report.txt # bug report file
./install.sh status
./install.sh uninstall --yes
```

`--method tarball` and `--method native` remain accepted only for compatibility with historical releases/workflows. They are not current release formats.

## Two scripts, different jobs

| Script            | Who runs it                                                               | What it does                                                                        |
| ----------------- | ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `bootstrap-install.sh` | You, via the one-liner above | Verifies the release-published installer before executing it |
| `install.sh` | Called by the bootstrap or run from a verified release asset | The menu: install, install an older release, uninstall, guided repair, bug report, status. Handles the package manager and swhkd |
| `install-user.sh` | Called by `install.sh`, or by you after a manual download or source build | Configures per-user install state: engine service, desktop entry, icons, legacy audio cleanup, and the audio snapshots |

`install-user.sh` is the low-level tool. `install.sh` is the smart wrapper that calls it when needed and handles the rest (package manager, swhkd, PipeWire services).

For a full uninstall through the same smart wrapper:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh | bash -s -- uninstall --yes
```

This removes managed per-user files first, then removes the native `linux-soundboard` package when one is installed. Add `--keep-package` to remove only the per-user setup.

### What uninstall does to your audio setup

Before it changes anything, an install records a snapshot of your audio state:
the default microphone and speakers, the engine service state, and a checksum of
every PipeWire, WirePlumber, and PulseAudio config file it can see. Snapshots live
in `~/.local/state/linux-soundboard/install-user/snapshots/`. The newest ten are
kept, plus the very first one — that is the only record of your setup before the
app was ever installed, and it is what uninstalling compares against.

Uninstalling prints what changed since that snapshot and asks **once** whether to
put it back:

```
Changes since install (2026-07-28T20:15:40+03:00):
  default_source_name: linuxsoundboard.virtual_mic -> alsa_input.pci-0000_12_00.6.analog-stereo
  engine_unit:         active/enabled -> inactive/disabled

Restore the audio setup recorded before Linux Soundboard was installed? [y/N]
```

Answering `n` leaves your current setup untouched. A non-interactive uninstall
(`--yes`, or no terminal) never changes your default device on its own; pass
`--restore-default-source` to opt in or `--keep-current-default-source` to be explicit.

You can inspect this at any time without uninstalling:

```bash
./packaging/linux/install-user.sh snapshot-diff
```

---

## Historical tarball install (2.4.4 and older)

For source builds or when you want to manage the download yourself, verify the
release file with the command above before extracting it.

### Step-by-step install

```bash
# 1. Download the latest release tarball from the Releases page
wget https://github.com/germanua/Linux-SoundBoard/releases/latest/download/linux-soundboard-2.4.4-linux-x86_64.tar.gz

# 2. Extract it
tar -xzf linux-soundboard-2.4.4-linux-x86_64.tar.gz
cd linux-soundboard-2.4.4-linux-x86_64

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
| Icons               | `~/.local/share/icons/hicolor/*/apps/{com.linuxsoundboard.app,linux-soundboard}.png` | Icon set for all sizes (both names installed) |
| Engine service      | `~/.config/systemd/user/linux-soundboard-engine.service`      | Starts the audio engine at login                    |
| Legacy cleanup      | Old PipeWire/PulseAudio/WirePlumber soundboard routing files  | Disables obsolete persistent virtual mic setup      |
| Microphone routing  | App setting in `~/.config/linux-soundboard/config.json`       | Routes recording apps while leaving system defaults alone by default |
| Settings            | `~/.config/linux-soundboard/config.json`                      | Application settings only                           |
| Sound library       | `~/.config/linux-soundboard/library.sqlite3`                  | Scanned folders, sounds, tabs, and hotkey bindings   |

The engine creates `Linux_Soundboard_Mic` at runtime while it is running. It uses low PipeWire priority, unmutes the virtual mic on registration, and claims the system default mic so recording apps use it automatically. Switch to **Manual** routing if you prefer to manage the default mic yourself.

### Installer commands

```bash
# Full-system wrapper commands
./install.sh repair
./install.sh status
./install.sh uninstall --yes
./install.sh uninstall --yes --keep-package

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

# Uninstall without restoring the previous default microphone
./install-user.sh remove --yes --keep-current-default-source
```

---

## Legacy native packages

Arch/AUR, Debian `.deb`, and RPM packages were public release formats through 2.4.4. They are retained in the repository for history and migration support, but the next public release is planned as AppImage-only.

If one is installed, remove it before using the prepared AppImage so `/usr/bin/linux-soundboard` does not shadow `~/.local/opt/linux-soundboard/linux-soundboard`. The top-level `install.sh` can detect and offer to remove these packages automatically.

For development from source, distro-specific build dependencies are still documented in the source-build section below; dropping native release packages does not drop support for those distributions.

## AppImage

The AppImage can install itself or run temporarily:

```bash
chmod +x linux-soundboard-x86_64.AppImage
./linux-soundboard-x86_64.AppImage
```

Before touching the audio graph, direct launch offers three choices:

- **Install for persistent virtual mic** copies the AppImage to `~/.local/opt/linux-soundboard/linux-soundboard`, registers the desktop entry and user service through the bundled `install-user.sh`, then connects the GUI to the matching service engine.
- **Run temporarily** creates no service. The in-process engine restores the previously recorded eligible microphone, or the best eligible hardware/enhancement source, before removing the temporary virtual mic on close.
- **Exit** changes no configuration, service, or audio-graph state.

The prompt returns on every direct launch until the AppImage is installed. Once a user installation exists, opening a newer downloaded AppImage updates that installed copy automatically, restarts the user engine, and launches the GUI without another choice or any terminal commands. The installed version marker prevents an older downloaded AppImage from silently downgrading a newer installation.

If AppImage reports a FUSE error:

```bash
# Ubuntu / Debian
sudo apt install libfuse2
# Fedora
sudo dnf install fuse-libs
# Arch
sudo pacman -Syu --needed fuse2
# openSUSE
sudo zypper install fuse
```

---

## Wayland and global hotkeys

On Wayland, Linux Soundboard uses `swhkd` for global hotkeys.
Upstream `swhkd` captures all keyboards visible to the daemon, so Linux Soundboard treats this integration as single-seat only.

**In-app install:** When the app detects that `swhkd` is missing or inactive, a banner appears with an **Install** button. The button only runs the fixed root-owned helper at `/usr/libexec/linux-soundboard/install-swhkd-helper.sh`; it never elevates a helper from an AppImage mount, `$HOME`, or `$PATH`. The one-command installer provisions this helper from the signed AppImage automatically on Wayland. A directly downloaded AppImage does not self-provision it; run the one-command installer or `install.sh repair` first.

Linux Soundboard checks the installed daemon before starting it. If the binary
contains swhkd's rfkill support, or cannot be inspected, it is not launched; the
same banner offers to rebuild and reinstall it safely. This also handles unsafe
`/usr/bin/swhkd` binaries left behind when Linux Soundboard itself is updated.

swhkd grabs your keyboards directly and re-emits the keys it does not claim
through a virtual keyboard, which the kernel's `uinput` module provides. Almost
every system already has it — the kernel autoloads the module when `/dev/uinput`
is opened — and those are left alone. Only where opening the node fails does the
app offer **Load uinput** alongside **Install without it**, so nothing is loaded
without you agreeing to it. See
[TROUBLESHOOTING.md](TROUBLESHOOTING.md#swhkd-fails-with-failed-to-create-uinput-device)
for the manual commands.

Requirements for the in-app install:

- The root-owned Linux Soundboard helper installed under `/usr/libexec/linux-soundboard`
- `pkexec` available (provided by `pkexec` on newer Debian/Ubuntu releases, `policykit-1` on older Debian/Ubuntu releases, or `polkit` on Fedora/Arch)
- Network access to fetch the pinned `swhkd` source commit from GitHub

**Manual install:** from a trusted Linux Soundboard source checkout, install `install-swhkd-helper.sh`, `build-swhkd-locked.sh`, and `swhkd-Cargo.lock.pinned` under `/usr/libexec/linux-soundboard`, then run the helper as root. The helper builds the pinned commit with the pinned lockfile and `no_rfkill` feature; do not promote a PATH-resolved `/usr/bin/swhkd` with `chmod u+s`.

On **X11 and XWayland**, the app uses a native XInput2 backend. No `swhkd` needed.

---

## Build from source

### Install build dependencies

**Arch:**

```bash
sudo pacman -Syu --needed cargo rust pkgconf gtk4 libadwaita \
  libpulse opus libx11 libxi pipewire pipewire-pulse wireplumber clang
```

**Debian / Ubuntu:**

```bash
sudo apt install build-essential cargo rustc pkg-config \
  libgtk-4-dev libadwaita-1-dev libpulse-dev libopus-dev \
  libpipewire-0.3-dev libx11-dev libxi-dev libclang-dev \
  pipewire pipewire-pulse wireplumber pulseaudio-utils
```

**Fedora:**

```bash
sudo dnf install cargo rust gcc gcc-c++ clang-devel pkgconf-pkg-config \
  gtk4-devel libadwaita-devel pulseaudio-libs-devel opus-devel \
  libX11-devel libXi-devel pipewire-devel pipewire pipewire-utils \
  pipewire-pulseaudio wireplumber pulseaudio-utils
```

### Build and install

```bash
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard/src
cargo build --release

# Install using the user installer, pointing it at the freshly built binary
cd ..
./packaging/linux/install-user.sh install ./target/release/linux-soundboard
```

The installer detects the binary next to the script automatically when run from the repository root.

After every rebuild, run the repair command with the exact new binary before testing the installed service:

```bash
./packaging/linux/install-user.sh repair ./target/release/linux-soundboard
```

Running `./target/release/linux-soundboard` directly is supported for development. If the installed engine is older or otherwise incompatible, the UI stops that service and uses its own in-process engine so that only one process owns the virtual microphone.

---

## After install: first launch checklist

1. Launch Linux Soundboard from your application menu or run `linux-soundboard` in a terminal.
2. Confirm PipeWire sees the virtual microphone:
   ```bash
   wpctl status -n | grep Soundboard
   ```
3. In Discord, OBS, Zoom, or your target application, select **Linux_Soundboard_Mic** as the input device when the app exposes a microphone picker.
4. Leave **Microphone Routing** set to **Default** (recommended). The soundboard claims the system default mic so apps use it automatically. Switch to **Manual** only if you manage the default mic yourself via pavucontrol or similar.
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
