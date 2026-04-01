# Troubleshooting

This guide covers the issues most likely to block installation, startup, audio routing, and hotkeys.

## Start With These Checks

```bash
cat /etc/os-release
echo "XDG_SESSION_TYPE=$XDG_SESSION_TYPE"
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
echo "DISPLAY=$DISPLAY"
systemctl --user status pipewire pipewire-pulse wireplumber
```

## Installation Problems

### AppImage fails with a FUSE error

Install the host FUSE package and retry:

- Ubuntu / Debian: `sudo apt install libfuse2`
- Fedora: `sudo dnf install fuse-libs`
- Arch Linux: `sudo pacman -S fuse2`
- openSUSE: `sudo zypper install fuse`

### `.deb` install reports dependency problems

Use APT to resolve dependencies instead of `dpkg -i` alone:

```bash
sudo apt install ./linux-soundboard_1.1.1-1_amd64.deb
```

If host audio packages are missing:

```bash
sudo apt install pipewire pipewire-pulse wireplumber pulseaudio-utils
```

### `.rpm` install reports missing dependencies

Install with DNF:

```bash
sudo dnf install ./linux-soundboard-1.1.1-1.x86_64.rpm
```

If the audio stack is missing:

```bash
sudo dnf install pipewire pipewire-pulseaudio wireplumber pulseaudio-utils
```

## Startup and UI Problems

### The application does not start in a graphical session

Confirm you are actually inside Wayland or X11:

```bash
echo "$XDG_SESSION_TYPE"
echo "$WAYLAND_DISPLAY"
echo "$DISPLAY"
```

If GTK startup is unstable in the current session, force the X11 path:

```bash
LSB_FORCE_X11=1 linux-soundboard
```

You can also test the toolkit backend directly:

```bash
GDK_BACKEND=x11 linux-soundboard
```

### VMware guest: UI stops reacting to clicks or memory spikes at startup

This is typically a GTK renderer problem inside the VM rather than a broken package.

Test the safer renderer path:

```bash
GSK_RENDERER=cairo linux-soundboard
```

If you also want the X11 backend:

```bash
LSB_FORCE_X11=1 GSK_RENDERER=cairo linux-soundboard
```

Newer builds automatically prefer `GSK_RENDERER=cairo` when a VMware guest is detected and no renderer override is already set.

### Settings or state are not saved

Inspect the config directory:

```bash
ls -la ~/.config/linux-soundboard/
```

If needed, fix ownership and permissions for the user running the app.

## Audio Problems

### Virtual microphone was not created

Check the audio stack first:

```bash
pgrep -x pipewire
pactl list short sources
```

Install and start the required services if they are missing:

```bash
systemctl --user enable --now pipewire pipewire-pulse wireplumber
```

Common package names:

- Debian / Ubuntu: `pipewire pipewire-pulse wireplumber pulseaudio-utils`
- Fedora: `pipewire pipewire-pulseaudio wireplumber pulseaudio-utils`
- Arch Linux: `pipewire pipewire-pulse wireplumber`

### Sounds play through speakers but not through Discord or OBS

Verify that the target app is listening to `Linux_Soundboard_Mic`, not your physical microphone.

List sources:

```bash
pactl list short sources
```

Then switch the target application input device to `Linux_Soundboard_Mic`.

### Mic passthrough does not work

Check three things:

1. Mic passthrough is enabled in Linux Soundboard settings.
2. The correct real microphone is selected.
3. The source exists in PipeWire:

```bash
pactl list short sources
```

## Hotkey Problems

### Wayland hotkeys do not work

Wayland global hotkeys depend on `swhkd`.

Confirm it is installed and running:

```bash
command -v swhkd
pgrep swhkd
```

Check the setuid bit on the installed binary:

```bash
ls -l "$(command -v swhkd)"
```

If needed:

```bash
sudo chmod u+s "$(command -v swhkd)"
```

Installation paths:

- Arch family: install `swhkd-bin` or `swhkd-git` from the AUR
- Debian / Ubuntu / Fedora / openSUSE: follow upstream installation guidance

Upstream guide:

- https://github.com/waycrate/swhkd/blob/main/INSTALL.md

### X11 hotkeys do not work

The built-in X11 backend is used on X11 and XWayland sessions. Verify the session type:

```bash
echo "$XDG_SESSION_TYPE"
```

If you are inside Wayland but want the X11 path, launch with:

```bash
LSB_FORCE_X11=1 linux-soundboard
```

## Build Problems

### Rust or Cargo is missing

Install Rust with rustup:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### GTK, Libadwaita, PulseAudio, X11, or ALSA development packages are missing

Use the dependency blocks in [INSTALL.md](INSTALL.md) under the source-build section.

### Linking fails because a C compiler is missing

- Debian / Ubuntu: `sudo apt install build-essential`
- Fedora: `sudo dnf install gcc gcc-c++ make`
- Arch Linux: `sudo pacman -S base-devel`

## When Reporting a Bug

Attach:

- distro and version
- session type
- install method
- exact error output
- `RUST_LOG=debug` logs if available
- whether the issue reproduces on both Wayland and X11
