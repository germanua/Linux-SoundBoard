# Installation

[Documentation](README.md) · [Getting started](GETTING_STARTED.md) · [Troubleshooting](TROUBLESHOOTING.md)

This guide describes the AppImage workflow in this source revision. The
[official Releases page](https://github.com/germanua/Linux-SoundBoard/releases)
is authoritative for published versions and files. Native packages and tarballs
from 2.4.4 and earlier are historical distributions.

**On this page:** [Requirements](#requirements) · [Install](#install-with-the-verified-bootstrap) ·
[Verify](#verify-a-download) · [Update](#update-or-repair) · [Remove](#uninstall) ·
[Hotkeys](#wayland-and-global-hotkeys) · [Source build](#build-from-source)

## Requirements

| Requirement | Current AppImage build |
| --- | --- |
| Architecture | x86_64 |
| C library | glibc 2.39 or newer; checked by the installer and AppImage preflight |
| Desktop | A graphical Wayland or X11 session |
| Audio | PipeWire, its PulseAudio compatibility service, and WirePlumber |
| Persistent engine | A working systemd user session |
| AppImage mounting | Host FUSE support; see [FUSE troubleshooting](TROUBLESHOOTING.md#appimage-fails-with-a-fuse-error) |

Ubuntu 24.04, Debian 13, and newer distributions can meet the glibc baseline;
meeting it alone is not a complete compatibility test. Older hosts such as Ubuntu
22.04 or Debian 12 do not meet that baseline. Check your host with `ldd --version`.
Do not replace a distribution's glibc manually to make an AppImage run.

## Install with the verified bootstrap

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh | bash
```

The bootstrap authenticates the release-published installer using the pinned
release key and signed checksum manifest. That installer verifies the AppImage,
checks the host, and installs it under your account. Missing signing assets,
invalid signatures, or checksum mismatches stop installation.

This command downloads and executes the bootstrap itself from the official
repository over HTTPS. The signature checks authenticate the subsequent release
assets; they are not a signature check of that initial shell script.

The AppImage is installed in `~/.local/opt/linux-soundboard/`. On Wayland, setup
of the fixed root-owned hotkey helper may request `sudo`. Removing a legacy
system package also requires the package manager's administrator permission.

### Existing AUR, DEB, or RPM installation

An old `/usr/bin/linux-soundboard` can shadow the per-user installation and leave
the GUI and engine on different versions. The installer detects a conflicting
native package and offers removal before installing the AppImage. Read that
prompt before accepting. Historical packages remain available where published;
this guide does not present them as current release formats.

## Verify a download

Replace `VERSION` with the version in the downloaded filename:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh   | bash -s -- verify ./linux-soundboard-VERSION-x86_64.AppImage
```

For a specific older release, append `--version vX.Y.Z`. Verification requires
that the selected release provides the manifest and signing assets expected by
the installer. An older release without them cannot be authenticated by this
workflow; a plain checksum alone does not establish its publisher.

The bootstrap obtains the current release's authenticated installer first,
including when you ask that installer to work with another version.

## Launch a downloaded AppImage

After verification:

```bash
chmod +x linux-soundboard-VERSION-x86_64.AppImage
./linux-soundboard-VERSION-x86_64.AppImage
```

| Choice | Result |
| --- | --- |
| Install for persistent virtual mic | Copies the AppImage into the user installation and configures its desktop entry and matching engine service. |
| Run temporarily | Runs an in-process engine for this launch; eligible routing state is restored when it shuts down. |
| Exit | Leaves installation and audio state unchanged. |

A direct download does not provision the privileged Wayland helper by itself.
Use the verified bootstrap or guided repair if the helper is missing.

For an installed AppImage, launch from the application menu or use:

```bash
"$HOME/.local/opt/linux-soundboard/linux-soundboard"
```

The installer does not guarantee a `linux-soundboard` command on `PATH`.

## Update or repair

Opening a newer downloaded AppImage updates an existing user installation and
restarts the matching engine. The installed version marker prevents silent
downgrades from an older downloaded AppImage.

For guided repair:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh   | bash -s -- fix
```

Repair can change the user service, hotkey helper, and audio-service state.
Finish active calls or recordings before running it. For inspection only, use
`status` instead of `fix`.

### Installer commands

The commands below assume `install.sh` is an already authenticated release asset.
You can also pass their arguments through the bootstrap as above.

| Command | Purpose |
| --- | --- |
| `./install.sh install` | Install the current AppImage release. |
| `./install.sh install --version vX.Y.Z` | Select a published version. |
| `./install.sh versions` | List published versions. |
| `./install.sh verify FILE --version vX.Y.Z` | Check a downloaded release file. |
| `./install.sh status` | Inspect installation state. |
| `./install.sh fix` | Run guided repair. |
| `./install.sh report --output report.txt` | Collect a local bug report. |
| `./install.sh uninstall --yes --keep-data` | Remove managed installation files and preserve library/configuration data. |

`bootstrap-install.sh` authenticates the release installer; `install.sh` manages
the workflow; the bundled `install-user.sh` manages per-user installation files.
Public source snapshots need not include private release-packaging tooling.

## Installed files and audio behavior

These paths assume default XDG locations and the ordinary public profile.

| Item | Location |
| --- | --- |
| Application | `~/.local/opt/linux-soundboard/linux-soundboard` |
| Desktop entry | `~/.local/share/applications/com.linuxsoundboard.app.desktop` |
| Application icons | `~/.local/share/icons/hicolor/` |
| User engine unit | `~/.config/systemd/user/linux-soundboard-engine.service` |
| Settings | `~/.config/linux-soundboard/config.json` |
| Library, folders, tabs, hotkeys | `~/.config/linux-soundboard/library.sqlite3` |
| Installation snapshots | `~/.local/state/linux-soundboard/install-user/snapshots/` |

The engine creates **Linux_Soundboard_Mic** while running. **Default** microphone
routing in the ordinary profile claims the system default input; **Manual**
leaves that choice to you. Pick the virtual microphone explicitly in the target
app when needed. See [Getting started](GETTING_STARTED.md).

## Uninstall

To remove managed files while preserving your settings and library:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh   | bash -s -- uninstall --yes --keep-data
```

The wrapper also removes a native `linux-soundboard` package if one is installed.
Add `--keep-package` if you intend to remove only the per-user setup.

Installation records audio-state snapshots before making changes. Interactive
removal compares with the original snapshot and asks whether to restore the
previous default microphone. Non-interactive removal does not opt into changing
your default device: add `--restore-default-source` to request restoration, or
`--keep-current-default-source` to explicitly retain the current choice.

Snapshots are diagnostic records, not a complete backup or rollback of every
PipeWire or WirePlumber configuration change. Keep your own audio-file and
profile backups. Do not delete profile data to repair an installation.

## Wayland and global hotkeys

Wayland uses a pinned, managed build of `swhkd`. It captures keyboards directly
and is treated as single-seat integration. The app checks the daemon before
launching it and rejects builds with unsafe or uninspectable rfkill support.

The in-app **Install** button only invokes the fixed root-owned helper at
`/usr/libexec/linux-soundboard/install-swhkd-helper.sh`. It does not elevate a
helper from an AppImage mount, your home directory, or `PATH`. The bootstrap
installation provisions that helper from the authenticated release.

The helper needs PolicyKit (`pkexec`) and network access to the pinned source.
If `/dev/uinput` cannot be opened, the setup flow offers a specific repair rather
than requiring unconditional kernel changes. Follow
[uinput troubleshooting](TROUBLESHOOTING.md#swhkd-fails-with-failed-to-create-uinput-device)
if that error appears. Do not fix an arbitrary `swhkd` binary with `chmod u+s`.

X11 uses the native XInput2 backend. Running through XWayland is an X11 fallback;
its shortcut behavior still depends on the surrounding desktop session.

## Build from source

The current [license](../LICENSE) permits private builds of unmodified source
for personal noncommercial use, and builds of proposed contributions through
[CONTRIBUTING.md](../CONTRIBUTING.md). It does not grant redistribution rights.
Use the terms shipped with the source revision you build.

The manifest requires Rust 1.85 or newer, GTK 4.10 or newer, and libadwaita 1.5
or newer. Your distribution's packaged compiler and libraries must meet the
locked dependency graph's requirements.

### Build dependencies

**Arch family:**

```bash
sudo pacman -Syu --needed base-devel rust pkgconf gtk4 libadwaita   libpulse opus libx11 libxi pipewire pipewire-pulse wireplumber clang
```

**Debian / Ubuntu:**

```bash
sudo apt install build-essential cargo rustc pkg-config   libgtk-4-dev libadwaita-1-dev libpulse-dev libopus-dev   libpipewire-0.3-dev libx11-dev libxi-dev libclang-dev   pipewire pipewire-pulse wireplumber pulseaudio-utils
```

**Fedora:**

```bash
sudo dnf install cargo rust gcc gcc-c++ clang-devel pkgconf-pkg-config   gtk4-devel libadwaita-devel pulseaudio-libs-devel opus-devel   libX11-devel libXi-devel pipewire-devel pipewire pipewire-utils   pipewire-pulseaudio wireplumber pulseaudio-utils
```

These commands install build prerequisites and may change system packages.
They are not needed just to run a compatible AppImage.

### Compile and run

```bash
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard
cargo build --release --locked
./target/release/linux-soundboard
```

For a managed persistent installation, use the signed AppImage. Running a source
build while another Linux Soundboard installation is active can affect the current
soundboard session because both use the same public application profile.

## Historical releases and Flatpak

Obtain old tarballs, AUR references, DEB, or RPM files from their specific
[release tag](https://github.com/germanua/Linux-SoundBoard/releases), then follow
the instructions and license accompanying that release. Do not combine a
`releases/latest` download URL with a hardcoded old filename.

Flatpak packaging experiments do not establish that a Flathub release exists or
that its sandbox supports the same installer and systemd integration. Use the
published release channels linked by the official project.
