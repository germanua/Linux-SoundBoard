# Troubleshooting Guide

This guide covers common issues you might encounter when installing or using Linux Soundboard, along with their solutions.

---

## Table of Contents

- [Installation Issues](#installation-issues)
  - [AppImage Issues](#appimage-issues)
  - [DEB Package Issues](#deb-package-issues)
  - [RPM Package Issues](#rpm-package-issues)
  - [Flatpak Issues](#flatpak-issues)
- [Build Issues](#build-issues)
- [Runtime Issues](#runtime-issues)
- [Audio Issues](#audio-issues)
- [Hotkey Issues](#hotkey-issues)
- [Distribution-Specific Issues](#distribution-specific-issues)

---

## Installation Issues

### AppImage Issues

#### ❌ Error: "fuse: failed to exec fusermount: No such file or directory"

**Problem:** FUSE2 is not installed on your system.

**Solution:**

```bash
# Ubuntu/Debian
sudo apt install libfuse2

# Fedora
sudo dnf install fuse-libs

# Arch Linux
sudo pacman -S fuse2

# openSUSE
sudo zypper install fuse
```

After installing, try running the AppImage again:
```bash
./linux-soundboard-x86_64.AppImage
```

---

#### ❌ Error: "Permission denied"

**Problem:** AppImage file is not executable.

**Solution:**

```bash
chmod +x linux-soundboard-x86_64.AppImage
./linux-soundboard-x86_64.AppImage
```

---

#### ⚠️ Warning: "pactl not found" or Virtual Mic Not Created

**Problem:** PulseAudio utilities are not installed (shouldn't happen with v1.1.0+ as pactl is bundled).

**Solution:**

```bash
# Ubuntu/Debian
sudo apt install pulseaudio-utils

# Fedora
sudo dnf install pulseaudio-utils

# Arch Linux
sudo pacman -S pulseaudio
```

---

#### ⚠️ AppImage Runs But No Virtual Microphone

**Problem:** PipeWire or WirePlumber is not running.

**Check if PipeWire is running:**
```bash
pgrep -x pipewire
```

**Solution:**

```bash
# Ubuntu/Debian
sudo apt install pipewire pipewire-pulse wireplumber
systemctl --user enable --now pipewire pipewire-pulse wireplumber

# Fedora
sudo dnf install pipewire pipewire-pulseaudio wireplumber
systemctl --user enable --now pipewire pipewire-pulse wireplumber

# Arch Linux
sudo pacman -S pipewire pipewire-pulse wireplumber
systemctl --user enable --now pipewire pipewire-pulse wireplumber
```

**Restart your session** after installing PipeWire.

---

### DEB Package Issues

#### ❌ Error: "dpkg: dependency problems"

**Problem:** Missing dependencies.

**Solution:**

```bash
# Install with automatic dependency resolution
sudo apt install -f ./linux-soundboard_1.1.0-1_amd64.deb

# Or install dependencies first
sudo apt update
sudo apt install libgtk-4-1 libadwaita-1-0 libpulse0 pipewire pipewire-pulse wireplumber pulseaudio-utils
```

---

#### ❌ Error: "Package architecture (amd64) does not match system"

**Problem:** You're trying to install an amd64 package on a different architecture.

**Solution:** Build from source or use the AppImage (which is also x86_64 only).

---

### RPM Package Issues

#### ❌ Error: "Failed dependencies"

**Problem:** Missing dependencies.

**Solution:**

```bash
# Install with automatic dependency resolution
sudo dnf install ./linux-soundboard-1.1.0-1.fc*.x86_64.rpm

# Or install dependencies first
sudo dnf install gtk4 libadwaita pulseaudio-libs pipewire pipewire-pulseaudio wireplumber pulseaudio-utils
```

---

#### ⚠️ SELinux Denials (Fedora)

**Problem:** SELinux is blocking the application.

**Check for denials:**
```bash
sudo ausearch -m avc -ts recent | grep linux-soundboard
```

**Temporary solution (for testing):**
```bash
sudo setenforce 0
```

**Permanent solution:**
```bash
# Re-enable SELinux
sudo setenforce 1

# Create custom policy (if needed)
sudo ausearch -m avc -ts recent | audit2allow -M linux-soundboard
sudo semodule -i linux-soundboard.pp
```

---

### Flatpak Issues

#### ❌ Error: "runtime org.gnome.Platform/x86_64/47 not installed"

**Problem:** GNOME runtime not installed.

**Solution:**

```bash
flatpak install flathub org.gnome.Platform//47
```

---

#### ⚠️ Virtual Microphone Not Working in Flatpak

**Problem:** Flatpak sandbox restrictions.

**Solution:** The Flatpak has proper permissions configured. If it still doesn't work:

```bash
# Check if PipeWire is running on host
systemctl --user status pipewire

# Restart PipeWire
systemctl --user restart pipewire pipewire-pulse
```

---

## Build Issues

### Missing Dependencies

#### ❌ Error: "cargo: command not found"

**Solution:**

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

---

#### ❌ Error: "gtk4-devel not found" or similar

**Solution:**

**Ubuntu/Debian:**
```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libpulse-dev libx11-dev libxi-dev pkg-config imagemagick
```

**Fedora:**
```bash
sudo dnf install gtk4-devel libadwaita-devel pulseaudio-libs-devel libX11-devel libXi-devel pkg-config ImageMagick alsa-lib-devel gcc gcc-c++ clang
```

**Arch Linux:**
```bash
sudo pacman -S gtk4 libadwaita libpulse libx11 libxi pkgconf imagemagick
```

---

#### ❌ Error: "failed to compile alsa-sys"

**Problem:** ALSA development libraries not installed.

**Solution:**

```bash
# Ubuntu/Debian
sudo apt install libasound2-dev

# Fedora
sudo dnf install alsa-lib-devel

# Arch Linux
sudo pacman -S alsa-lib
```

---

#### ❌ Error: "linking with `cc` failed"

**Problem:** C compiler or linker not found.

**Solution:**

```bash
# Ubuntu/Debian
sudo apt install build-essential

# Fedora
sudo dnf install gcc gcc-c++ make

# Arch Linux
sudo pacman -S base-devel
```

---

### Rust Version Issues

#### ❌ Error: "requires rustc 1.85 or newer"

**Solution:**

```bash
# Update Rust
rustup update stable
rustup default stable

# Verify version
rustc --version
```

---

## Runtime Issues

### Application Won't Start

#### ❌ Error: "Failed to initialize GTK"

**Problem:** GTK4 or display server issue.

**Check display server:**
```bash
echo $WAYLAND_DISPLAY
echo $DISPLAY
```

**Solution:**

1. Make sure you're running in a graphical session
2. Try forcing X11 mode:
   ```bash
   LSB_FORCE_X11=1 linux-soundboard
   ```
3. Check GTK4 installation:
   ```bash
   # Ubuntu/Debian
   sudo apt install libgtk-4-1

   # Fedora
   sudo dnf install gtk4
   ```

---

#### ❌ Crash on Startup in a Wayland Session

**Problem:** Local compositor or GTK stack issue in your session.

**Solution:**

The app supports Wayland natively, but you can temporarily fall back to X11:
```bash
GDK_BACKEND=x11 linux-soundboard
```

Or set permanently:
```bash
export LSB_FORCE_X11=1
```

---

### Configuration Issues

#### ⚠️ Settings Not Saved

**Problem:** Configuration directory not writable.

**Check permissions:**
```bash
ls -la ~/.config/linux-soundboard/
```

**Solution:**

```bash
# Fix permissions
chmod 755 ~/.config/linux-soundboard/
chmod 644 ~/.config/linux-soundboard/*.json
```

---

## Audio Issues

### Virtual Microphone Not Created

#### ❌ Error: "Failed to create virtual microphone"

**Diagnosis:**

```bash
# Check if PipeWire is running
pgrep -x pipewire

# Check if pactl works
pactl list short sources

# Check for existing virtual mic
pactl list sources | grep Linux_Soundboard
```

**Solution:**

1. **Install PipeWire:**
   ```bash
   # Ubuntu/Debian
   sudo apt install pipewire pipewire-pulse wireplumber

   # Fedora
   sudo dnf install pipewire pipewire-pulseaudio wireplumber
   ```

2. **Enable and start PipeWire:**
   ```bash
   systemctl --user enable --now pipewire pipewire-pulse wireplumber
   ```

3. **Restart your session** (logout and login)

4. **Verify PipeWire is running:**
   ```bash
   systemctl --user status pipewire
   ```

---

### No Audio Output

#### ⚠️ Sounds Don't Play

**Check:**

1. **Volume levels:**
   - Check application volume slider
   - Check system volume
   - Check if muted

2. **Audio device:**
   ```bash
   pactl list short sinks
   ```

3. **Test system audio:**
   ```bash
   paplay /usr/share/sounds/alsa/Front_Center.wav
   ```

**Solution:**

```bash
# Restart PipeWire
systemctl --user restart pipewire pipewire-pulse

# Check default sink
pactl info | grep "Default Sink"

# Set default sink if needed
pactl set-default-sink <sink-name>
```

---

### Microphone Passthrough Not Working

#### ⚠️ Real Mic Not Mixed with Soundboard

**Check:**

1. **Mic passthrough enabled** in settings
2. **Correct source selected** in settings
3. **Source exists:**
   ```bash
   pactl list short sources
   ```

**Solution:**

1. Open Settings in Linux Soundboard
2. Enable "Mic Passthrough"
3. Select your real microphone from dropdown
4. Test in Discord/OBS

---

## Hotkey Issues

### Global Hotkeys Not Working

#### ⚠️ Hotkeys Don't Trigger

**Diagnosis:**

Check which display server you're using:
```bash
echo $WAYLAND_DISPLAY  # If set, you're on Wayland
echo $DISPLAY          # If set, X11 is available
```

**Wayland and X11 hotkey paths:**

**1. Wayland sessions: verify `swhkd` is installed and running**
  ```bash
  # Check if swhkd is installed
  which swhkd

  # Check if swhkd is running
  pgrep swhkd

  # Arch users can install:
  # yay -S swhkd-bin
  #
  # Debian/Ubuntu/Fedora users should follow:
  # https://github.com/waycrate/swhkd/blob/main/INSTALL.md

  # If not running, the application will start it automatically
  # Verify setuid bit is set (should be done by package installation)
  ls -l "$(command -v swhkd)"  # Should show 'rws' permissions
  ```

**2. Manual `swhkd` configuration (if needed)**
  ```bash
  # Set setuid bit on swhkd
  sudo chmod u+s "$(command -v swhkd)"

  # Restart the application
  ```

**3. Check `swhkd` logs**
  ```bash
  # View swhkd logs
  journalctl -xe | grep swhkd

  # Or check the config file
  cat ~/.config/swhkd/swhkdrc
  ```

**4. X11 and XWayland sessions: use the native X11 backend**
  - Native X11 hotkeys work on X11 directly
  - XWayland lets you use the X11 backend from a Wayland session when needed
  - Install XWayland if needed:
    ```bash
    # Ubuntu/Debian
    sudo apt install xwayland

    # Fedora
    sudo dnf install xorg-x11-server-Xwayland
    ```


---

#### ❌ Error: "Failed to register hotkey"

**Problem:** Hotkey already in use by another application.

**Solution:**

1. Choose a different key combination
2. Check for conflicts:
   ```bash
   # GNOME
   gsettings list-recursively org.gnome.desktop.wm.keybindings

   # KDE
   kreadconfig5 --file kglobalshortcutsrc
   ```

---

## Distribution-Specific Issues

### Ubuntu 22.04 / Debian 12

#### ⚠️ GTK4/Libadwaita Too Old

**Problem:** System GTK4/Libadwaita version is too old for source builds.

**Solution:** Use pre-built packages instead:
- ✅ DEB package (recommended)
- ✅ AppImage
- ✅ Flatpak

---

### Fedora Silverblue / Kinoite

#### ⚠️ Can't Install RPM

**Problem:** Immutable filesystem.

**Solution:** Use Flatpak (recommended for immutable distros):

```bash
flatpak install linux-soundboard-1.1.0.flatpak
```

Or layer the RPM:
```bash
rpm-ostree install ./linux-soundboard-1.1.0-1.fc*.x86_64.rpm
systemctl reboot
```

---

### Arch Linux

#### ⚠️ AUR Build Fails

**Solution:**

```bash
# Update system first
sudo pacman -Syu

# Clear package cache
rm -rf ~/.cache/yay/linux-soundboard-git

# Try again
yay -S linux-soundboard-git
```

---

## Still Having Issues?

### Enable Debug Logging

Run with debug output:
```bash
RUST_LOG=debug linux-soundboard 2>&1 | tee soundboard-debug.log
```

### Check System Information

```bash
# Distribution
cat /etc/os-release

# Kernel
uname -a

# Display server
echo "Wayland: $WAYLAND_DISPLAY"
echo "X11: $DISPLAY"

# PipeWire status
systemctl --user status pipewire

# Audio devices
pactl list short sinks
pactl list short sources
```

### Report a Bug

If none of the solutions work, please report an issue:

1. Go to: https://github.com/germanua/Linux-SoundBoard/issues
2. Click "New Issue"
3. Include:
   - Your distribution and version
   - Installation method (AppImage/DEB/RPM/Flatpak)
   - Error messages
   - Debug log (if applicable)
   - Steps to reproduce

---

## Quick Reference

### Common Commands

```bash
# Check PipeWire status
systemctl --user status pipewire

# Restart PipeWire
systemctl --user restart pipewire pipewire-pulse

# List audio sources
pactl list short sources

# List audio sinks
pactl list short sinks

# Check for virtual mic
pactl list sources | grep Linux_Soundboard

# Run with debug logging
RUST_LOG=debug linux-soundboard

# Force X11 mode
LSB_FORCE_X11=1 linux-soundboard

# Check Wayland hotkey daemon
pgrep swhkd

# Check swhkd permissions
ls -l "$(command -v swhkd)"
```

---

**Last Updated:** 2026-03-28
**Version:** 1.1.0 packaging refresh
