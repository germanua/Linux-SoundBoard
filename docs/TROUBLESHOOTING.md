# Troubleshooting

This guide covers the issues most likely to block installation, startup, audio routing, and hotkeys.

## Start With These Checks

```bash
cat /etc/os-release
echo "XDG_SESSION_TYPE=$XDG_SESSION_TYPE"
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
echo "DISPLAY=$DISPLAY"
systemctl --user status pipewire wireplumber
wpctl status -n
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
sudo apt install ./linux-soundboard_2.0.0-1_amd64.deb
```

If host audio packages are missing:

```bash
sudo apt install pipewire wireplumber
```

### `.rpm` install reports missing dependencies

Install with DNF:

```bash
sudo dnf install ./linux-soundboard-2.0.0-1.x86_64.rpm
```

If the audio stack is missing:

```bash
sudo dnf install pipewire wireplumber
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
pw-cli info 0
wpctl status -n
```

Install and start the required services if they are missing:

```bash
systemctl --user enable --now pipewire wireplumber
```

Common package names:

- Debian / Ubuntu: `pipewire wireplumber`
- Fedora: `pipewire wireplumber`
- Arch Linux: `pipewire wireplumber`

### Sounds play through speakers but not through Discord or OBS

Verify that the target app is listening to `Linux Soundboard Mic`, not your physical microphone.

List sources and the current default:

```bash
wpctl status -n
wpctl inspect @DEFAULT_SOURCE@
```

User installs set `Linux Soundboard Mic` as the system default microphone. If the target app is a game that only reads the default source, keep `Default Microphone` set to `Auto While Running`.

### `Linux Soundboard Mic` does not appear in the device list

The app installs a PipeWire config that creates one persistent `Audio/Source/Virtual` node at session start: `linuxsoundboard.virtual_mic`, displayed as `Linux Soundboard Mic`. Native packages drop the file at `/usr/share/pipewire/pipewire.conf.d/99-linuxsoundboard.conf`; AppImage builds write the same file to `~/.config/pipewire/pipewire.conf.d/` on first launch.

If the device is missing:

1. Confirm the file exists in either of those locations.
2. Restart PipeWire user services:
   ```bash
   systemctl --user restart wireplumber pipewire-pulse pipewire
   ```
3. Run `wpctl status -n | grep -i linuxsoundboard` and confirm `linuxsoundboard.virtual_mic` appears under Sources. If it is missing, check `journalctl --user -u pipewire -n 50` for parse errors in the conf.

If `systemctl --user` is unavailable (e.g. WSL2, container without user systemd), log out and log back in instead. The app falls back to an in-process virtual source automatically when the persistent node cannot be registered, so audio routing continues to work — but in that fallback mode the soundboard must be launched before the game.

### Discord plays my voice but soundboard clips do not reach it

Both go through the same `Linux Soundboard Mic`, so a partial failure usually means the feeder stream did not connect to the virtual mic input ports. Check:

1. With the soundboard running, run:
   ```bash
   pw-link -l | grep -i linuxsoundboard
   ```
   You should see `linuxsoundboard.virtual_mic_feeder:output_FL` linked into `linuxsoundboard.virtual_mic:input_FL` and `output_FR` linked into `input_FR`. If those links are missing, restart the soundboard and check the app logs.

2. The soundboard re-attaches the feeder automatically when PipeWire restarts, so a manual relaunch is not required. If it does not, restart the soundboard.

### EasyEffects (or another virtual source) is missing from the soundboard's "Mic source" dropdown

The dropdown lists every input source PipeWire reports — physical mics and modern virtual sources alike. If EasyEffects is running but absent, confirm its node has class `Audio/Source/Virtual` (older PipeWire builds use a different class):

```bash
pw-cli list-objects Node | grep -B2 -A8 easyeffects_source
```

Look for `media.class = "Audio/Source/Virtual"`. If it is `Stream/Output/Audio` or anything else, the version of EasyEffects you are running predates the Audio/Source/Virtual convention and cannot be picked as a passthrough source — upgrade EasyEffects.

### Mic passthrough does not work

Check three things:

1. Mic passthrough is enabled in Linux Soundboard settings.
2. The correct real microphone is selected.
3. The source exists in PipeWire:

```bash
wpctl status -n
```

## Hotkey Problems

### Wayland hotkeys do not work

Wayland global hotkeys depend on `swhkd`.

Try the in-app one-click flow first:

1. Open Linux Soundboard.
2. Click `Install` from the hotkey banner, hotkey settings page, or failed hotkey dialog.
3. Approve the privilege prompt.

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

If one-click install fails:

- Ensure PolicyKit is installed (`policykit-1` on Debian/Ubuntu, `polkit` on Fedora/Arch/openSUSE).
- Ensure network access is available (installer clones upstream `swhkd` sources).
- Retry from the app and review the detailed failure output shown in the dialog.

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

### GTK, Libadwaita, PipeWire, X11, or ALSA development packages are missing

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
