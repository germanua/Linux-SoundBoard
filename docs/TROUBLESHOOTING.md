# Troubleshooting

This guide covers the issues most likely to block installation, startup, audio routing, and hotkeys.

## Start With These Checks

The installer can run these for you and repair what it finds:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/install.sh | bash
```

Choose **Fix setup problems** (or run `./install.sh fix`). It repairs the user
install, the engine service, swhkd on Wayland, and the PipeWire services, printing
which step failed. If a step fails it offers to write a bug report.

To check by hand:

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
sudo apt install ./linux-soundboard_2.2.1-1_amd64.deb
```

If host audio packages are missing:

```bash
sudo apt install pipewire wireplumber
```

### `.rpm` install reports missing dependencies

Install with DNF:

```bash
sudo dnf install ./linux-soundboard-2.2.1-1.x86_64.rpm
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

Verify that the target app is listening to `Linux_Soundboard_Mic`, not your physical microphone.

List sources and the current default:

```bash
wpctl status -n
wpctl inspect @DEFAULT_SOURCE@
```

In the default **Microphone Routing → Default** mode, Linux Soundboard claims the system default mic so recording apps use `Linux_Soundboard_Mic` automatically. If you prefer to manage the default mic yourself, switch to **Manual** mode.

### `Linux_Soundboard_Mic` does not appear in the device list

The app no longer installs a persistent PipeWire config. `linuxsoundboard.virtual_mic`, displayed as `Linux_Soundboard_Mic`, is created by the running audio engine. If the engine autostarts at login, the mic can appear after login even when the UI is closed.

If the device is missing:

1. Confirm the engine is running:
   ```bash
   systemctl --user status linux-soundboard-engine.service
   ```
2. Restart the engine:
   ```bash
   systemctl --user restart linux-soundboard-engine.service
   ```
3. Run `wpctl status -n | grep -i linuxsoundboard` and confirm `linuxsoundboard.virtual_mic` appears under Sources.
4. If an old persistent config still exists, disable it and restart audio services:
   ```bash
   mv ~/.config/pipewire/pipewire.conf.d/99-linuxsoundboard.conf ~/.config/pipewire/pipewire.conf.d/99-linuxsoundboard.conf.disabled
   systemctl --user restart wireplumber pipewire-pulse pipewire
   ```

If `systemctl --user` is unavailable (e.g. WSL2, container without user systemd), keep the app running before launching the target app.

### Discord plays my voice but soundboard clips do not reach it

Both go through the same runtime `Linux_Soundboard_Mic`, so a partial failure usually means the target app is not routed to `linuxsoundboard.virtual_mic`, the virtual mic is muted by stale WirePlumber state, or the running engine is stale.

1. With the soundboard running, run:
   ```bash
   wpctl status -n | grep -i linuxsoundboard
   pw-metadata -n default -m | grep -i linuxsoundboard
   ```
   The virtual mic should not be muted, and the target app stream should point at `linuxsoundboard.virtual_mic`.

2. Inspect the expected and running app versions, protocol, schema, binary paths, PID, and service state:
   ```bash
   linux-soundboard --diagnose
   ```

3. If `compatibility` is `INCOMPATIBLE`, repair the installed binary and service:
   ```bash
   ./packaging/linux/install-user.sh repair ./target/release/linux-soundboard
   ```

During a v2.0→v2.1 package upgrade, `/usr/bin/linux-soundboard` can be replaced while the already-running engine still executes the old mapped binary. Version 2.1.1 stops that stale engine, reloads and restarts the user service once, and connects only after protocol, schema, and app version all match. If the restarted process is still stale, it is stopped before one transient local fallback starts. Do not run a second `--audio-engine` process manually.

Useful stale-engine checks:

```bash
linux-soundboard --diagnose
systemctl --user show linux-soundboard-engine.service -p MainPID -p ExecStart -p FragmentPath
readlink -f /proc/$(systemctl --user show linux-soundboard-engine.service -p MainPID --value)/exe
```

### Engine update failed

When Linux Soundboard finds an older or otherwise incompatible running engine,
it normally stops that process, reloads the user service, starts the engine
from the newly installed application, and reconnects automatically. A
successful update dialog shows the previous and current versions.

If the update dialog reports a failure, the app has already stopped the failed
service before starting one temporary engine for the current GUI session. Do
not start another `--audio-engine` process.

1. Close Linux Soundboard, then inspect the installed binary and service:
   ```bash
   linux-soundboard --diagnose
   systemctl --user status linux-soundboard-engine.service --no-pager
   journalctl --user -u linux-soundboard-engine.service -n 100 --no-pager
   ```
2. Reload the installed unit and retry it:
   ```bash
   systemctl --user daemon-reload
   systemctl --user restart linux-soundboard-engine.service
   linux-soundboard --diagnose
   ```
3. If diagnostics still report `INCOMPATIBLE`, reinstall the same
   `linux-soundboard` package through your package manager, or rerun the newer
   downloaded AppImage. Then launch Linux Soundboard again.

If `systemctl --user` reports that no user bus or manager is available, the
temporary engine is the safe fallback. Fix the user systemd session or continue
running the GUI while using the virtual microphone.

### Recover a schema-6 configuration backup

The first valid schema-6 load by v2.1.1 creates an exact private backup at `~/.config/linux-soundboard/config.json.pre-v6-backup`. If migration cannot proceed, the GUI and engine fail closed and leave the original file unchanged.

Close the GUI and stop every newer engine before recovery:

```bash
systemctl --user stop linux-soundboard-engine.service
pgrep -af linux-soundboard   # this must print no remaining GUI or engine process
cp -p ~/.config/linux-soundboard/config.json.pre-v6-backup \
  ~/.config/linux-soundboard/config.json
chmod 600 ~/.config/linux-soundboard/config.json
```

Then start the service or GUI again. Do not copy the backup while a newer process is running, because it may save migrated state over the restored file.

### The sound library moved out of config.json in v2.2.0

Sounds, folders, tab membership, and hotkey bindings now live in
`~/.config/linux-soundboard/library.sqlite3`. `config.json` keeps settings only.

The first v2.2.0 launch migrates an existing configuration and preserves the
original as `~/.config/linux-soundboard/config.json.pre-v8-backup` with `0600`
permissions. Both files matter when you back up or move a profile; copying
`config.json` alone no longer carries the library.

### Sound library could not be opened

Startup shows this when the database is missing, unreadable, or does not match
the expected schema. If a pre-v8 backup exists, the dialog offers **Restore
pre-v8 backup**, which archives the current `config.json` and `library.sqlite3`
as `.pre-restore-<id>` files, restores the backed-up configuration, and migrates
it again. Nothing is deleted.

Inspect the state before deciding:

```bash
systemctl --user stop linux-soundboard-engine.service
pgrep -af linux-soundboard   # this must print no remaining GUI or engine process
ls -l ~/.config/linux-soundboard/
sqlite3 ~/.config/linux-soundboard/library.sqlite3 'PRAGMA integrity_check;'
```

`integrity_check` printing `ok` means the file is intact and the failure is
elsewhere; check `journalctl --user -u linux-soundboard-engine.service` and the
GUI output. If the database is corrupt and no backup is offered, move it aside
and start over:

```bash
mv ~/.config/linux-soundboard/library.sqlite3{,.corrupt}
```

The next launch starts with an empty library. The scanned folders are stored in
that database as well, so add them again under `Settings` → `General` →
`Sound Folders`; tabs, hotkey bindings, and folder customizations do not come
back. No audio file on disk is affected.

### AppImage temporary versus installed behavior

A direct AppImage asks before changing audio state only when no stable user installation exists. **Install for persistent virtual mic** deploys the AppImage to the stable user path and keeps the service and virtual mic alive after GUI close. After installation, opening a newer downloaded AppImage updates the stable copy and restarts the matching engine automatically; no `systemctl` or file-copy command is needed. **Run temporarily** creates no service and restores an eligible previous/default microphone before removing its virtual mic. **Exit** makes no configuration, service, or audio-graph change.

### Adwaita warnings when launching from a terminal

`AdwBreakpointBin ... does not have a minimum size` indicates an application layout bug and should not appear in current builds.

`Using GtkSettings:gtk-application-prefer-dark-theme with libadwaita is unsupported` comes from a global GTK4 setting, not Linux Soundboard's theme selector. Linux Soundboard uses `AdwStyleManager`. Remove the legacy `gtk-application-prefer-dark-theme` entry from your GTK4 `settings.ini` if you want to silence the host warning; do not replace it with application launch scripts that force GTK themes.

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

### Discord whole-system-audio screen share is silent / breaks while Soundboard is running

In **Default** routing mode, the soundboard claims the system default mic. Screen-share system-audio capture uses a different PipeWire mechanism (sink monitors) that should not be affected by the default source setting.

If screen sharing breaks, run the diagnostic to inspect the PipeWire graph:

```bash
linux-soundboard --diagnose
```

Check whether the screen-share stream is connected to the soundboard's virtual mic or to a sink monitor. If the soundboard is interfering, switch to **Manual** routing mode as a workaround and configure the target app to use `Linux_Soundboard_Mic` directly.

### Mic passthrough captures a Vencord/Discord screenshare source instead of my microphone

Older builds could auto-select a screenshare virtual source — for example `vencord-screen-share` — for mic passthrough when no enhancement chain (EasyEffects/NoiseTorch) was running. The Passthrough Status would read `Active: vencord-screen-share` and your voice would be missing.

`Auto-detect (Default)` now only auto-selects a recognised enhancement chain or a real hardware microphone; it never auto-selects screenshare or other unrecognised virtual sources. If you still see one selected, it is almost certainly an **explicit** pick saved in your settings — `Auto-detect` only auto-picks, it never overrides a source you chose by hand. Check it:

```bash
grep mic_source ~/.config/linux-soundboard/config.json
```

If `mic_source` names the screenshare source (rather than being `null`), set **Settings → Microphone Source** back to `Auto-detect (Default)`, or pick your real microphone. Note that explicit selection of a screenshare source is intentionally still allowed for power users who want it.

### An app I don't want using Linux_Soundboard_Mic

In **Default** mode, the soundboard is the system default mic — all apps that use the default input will get it. To exclude a specific app, either:

1. Switch to **Manual** routing mode and configure only the apps you want to use `Linux_Soundboard_Mic` via pavucontrol or your app's input device picker
2. Stay in **Default** mode and change the unwanted app's input device to your physical microphone in pavucontrol

### Capturing routing audit data (debug aid for screen-share-with-sound regressions)

If Discord/Vesktop screen-share with sound works in **Manual** mode but breaks in **Default** mode, capture a comparison bundle so the routing-decision history can be reviewed. The audit log records every PipeWire metadata write Soundboard makes and every default-source command it issues, with timestamps. It only writes when `LSB_ROUTE_AUDIT=1` is set.

The recipe assumes you have a path to the Soundboard binary (e.g. `~/AppImage/linux-soundboard.AppImage` or the cargo build at `target/release/linux-soundboard`). Replace `LINUX_SOUNDBOARD` below with that path.

```bash
# 1. Stop the systemd-managed engine, the UI, Discord, and Vesktop. The audit
#    log only attaches to a fresh process started with the env var set.
systemctl --user stop linux-soundboard-engine.service 2>/dev/null
pkill -f linux-soundboard 2>/dev/null
pkill -f Discord 2>/dev/null
pkill -f vesktop 2>/dev/null

mkdir -p /tmp/lsb-bug
LINUX_SOUNDBOARD=target/release/linux-soundboard

# 2. Capture baseline state — no Soundboard, no share.
$LINUX_SOUNDBOARD --diagnose-graph-snapshot /tmp/lsb-bug/00-baseline.jsonl

# 3. Start Vesktop, join a voice channel, start "Share Sound" (verify it
#    works in this state). Capture the working state.
$LINUX_SOUNDBOARD --diagnose-graph-snapshot /tmp/lsb-bug/01-share-working.jsonl
pw-link -l > /tmp/lsb-bug/01-share-working.links

# 4. Stop screen-share (don't close Vesktop). Start Soundboard with audit
#    logging enabled. Set Microphone Routing to "Default".
LSB_ROUTE_AUDIT=1 $LINUX_SOUNDBOARD &
sleep 5

# 5. Capture state — Soundboard is up, no share yet.
$LINUX_SOUNDBOARD --diagnose-graph-snapshot /tmp/lsb-bug/02-soundboard-up.jsonl
pw-link -l > /tmp/lsb-bug/02-soundboard-up.links

# 6. THE TRIGGER — start "Share Sound" in Vesktop now. Wait 5 s. Capture
#    broken state.
sleep 5
$LINUX_SOUNDBOARD --diagnose-graph-snapshot /tmp/lsb-bug/03-share-broken.jsonl
pw-link -l > /tmp/lsb-bug/03-share-broken.links

# 7. Bundle the audit log (lives in $XDG_RUNTIME_DIR by default; falls back
#    to /tmp) and the four snapshots.
AUDIT_LOG="${XDG_RUNTIME_DIR:-/tmp}/linux-soundboard-route-audit.log"
cp "$AUDIT_LOG" /tmp/lsb-bug/audit.log
tar czf /tmp/lsb-bug.tar.gz -C /tmp lsb-bug
echo "bundle ready: /tmp/lsb-bug.tar.gz"
```

Useful diff commands afterwards:

```bash
diff -u /tmp/lsb-bug/02-soundboard-up.links /tmp/lsb-bug/03-share-broken.links
jq -c '.' /tmp/lsb-bug/audit.log     # validates each line is valid JSON
jq -c 'select(.kind | startswith("default_source"))' /tmp/lsb-bug/audit.log
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

If `swhkd` prints `Make sure to launch the binary with pkexec`, or the
permissions do not show an `s` bit on the owner execute field, repair the
install:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/install.sh | bash -s -- repair
```

Or apply the permission fix directly:

```bash
sudo chown root:root "$(command -v swhkd)"
sudo chmod u+s "$(command -v swhkd)"
```

If one-click install fails:

- Ensure PolicyKit is installed (`pkexec` on newer Debian/Ubuntu releases, `policykit-1` on older Debian/Ubuntu releases, `polkit` on Fedora/Arch/openSUSE).
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
