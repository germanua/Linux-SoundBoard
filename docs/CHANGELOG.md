# Changelog

All notable changes to Linux Soundboard will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [1.1.1] - 2026-03-26

### Fixed

- **Single-Key Hotkeys**: swhkd now supports single-key hotkeys without modifiers (e.g., `print`, `f1`, `space`, `escape`)
- **Hotkey Validation**: Removed incorrect validation that required at least one modifier key
- **Numpad Key Format**: Fixed swhkd config to use uppercase X11/evdev key names (KP_Divide, KP_Multiply, KP_Add, etc.) instead of lowercase. This fixes hotkey registration for numpad operator keys.

### Technical Details

- swhkd requires uppercase X11/evdev key names for numpad keys
- Correct format: `KP_Divide`, `KP_Multiply`, `KP_Subtract`, `KP_Add`, `KP_Enter`, `KP_Decimal`
- Numpad numbers use `KP_0` through `KP_9`
- Added comprehensive tests for single-key, combination, numpad operator, and numpad number hotkeys
- Updated SWHKD_MIGRATION.md documentation with correct key format
- Cleaned up unused code warnings

### Changed

- Documentation and packaging metadata now consistently describe the current hotkey model: native Wayland support via `swhkd`, plus full native X11 support
- Release packaging notes now call out native Wayland and X11 support across AppImage, DEB, RPM, Flatpak, and AUR packages

---

## [1.1.0] - 2026-03-24

### Added

- **Native Wayland Support**: AppImage and all packages now support Wayland with automatic X11 fallback
- **DEB Packages**: Official Debian/Ubuntu packages with automatic dependency management
- **RPM Packages**: Official Fedora/RHEL packages with automatic dependency management
- **Flatpak Support**: Universal Flatpak package with GNOME 47 runtime, ready for Flathub
- **Bundled pactl**: AppImage now includes pactl binary for guaranteed virtual microphone support
- **Dependency Checker**: Automatic preflight checks with helpful installation instructions
- **Build Scripts**: Comprehensive build scripts for Fedora and other distributions
- **Documentation**: Added TROUBLESHOOTING.md, PACKAGING.md, and improved README
- **CI/CD Automation**: GitHub Actions workflows for building all package types
- **Release Automation**: Automated release workflow with artifact uploads

### Fixed

- **AppImage Wayland Compatibility**: Removed forced X11 backend that broke Wayland-only systems
- **Virtual Microphone Creation**: Fixed issues on modern distributions (Ubuntu 24.04, Fedora 40)
- **Missing Dependencies**: Better error messages and automatic dependency checking
- **Build Errors**: Fixed missing ALSA dependencies and compilation issues
- **RPM Packaging**: Fixed metainfo.xml path in RPM spec file
- **Hotkey Model**: Restored missing HotkeyCode and functions after refactor

### Changed

- **Project Structure**: Reorganized with docs/, scripts/, and assets/ directories
- **AppImage Backend Detection**: Smart display server detection (Wayland preferred, X11 fallback)
- **CI Workflow**: Enhanced to build all package types (AppImage, DEB, RPM, Flatpak)
- **Documentation**: Improved README with better installation instructions
- **Version**: Bumped to 1.1.0 in Cargo.toml

### Improved

- **Error Messages**: More helpful error messages with installation instructions
- **Build Process**: Faster builds with better dependency management
- **User Experience**: Easier installation across all major distributions
- **Maintainer Experience**: Comprehensive packaging guide (PACKAGING.md)

---

## [1.0.0] - 2026-03-22

### Added

- Initial release of Linux Soundboard
- **Virtual Microphone**: Automatic virtual audio device creation for routing to Discord, OBS, Zoom
- **Mic Passthrough**: Mix real microphone with soundboard audio
- **LUFS Normalization**: Auto-gain for consistent volume levels
- **Global Hotkeys**: Universal hotkey support via swhkd (works on Wayland, X11, and TTY) with X11 fallback
- **Sound Library**: Organized tabs, folder sync, drag-and-drop support
- **Modern UI**: GTK4 + Libadwaita with dark/light theme support
- **Audio Processing**: Static and dynamic normalization modes
- **Independent Volume Control**: Separate sliders for speakers and virtual mic
- **Diagnostics**: Built-in memory monitoring and audio status tracking
- **AUR Package**: Available on Arch Linux via `linux-soundboard-git`
- **AppImage**: Portable package for universal Linux compatibility

### Technical Details

- Built with Rust 1.85+
- GTK4 and Libadwaita for native Linux UI
- Rodio + Symphonia for audio playback (MP3, WAV, OGG, FLAC, AAC)
- PulseAudio/PipeWire integration for virtual microphone
- swhkd for Wayland/TTY hotkeys with X11/XInput2 fallback on X11
- EBU R128 loudness measurement for normalization

---

## Release Notes

### v1.1.0 Highlights

This release focuses on **distribution compatibility** and **Wayland support**:

- 🎯 **Works on Fedora, Ubuntu, Debian** with native packages
- 🖥️ **Native Wayland support** - no more forced X11!
- 📦 **Multiple package formats** - choose what works best for you
- 🔧 **Better error messages** - know exactly what's missing
- 🚀 **Automated builds** - CI/CD for all package types

### Migration from 1.0.0

No breaking changes! Simply update to the new version:

**Arch Linux (AUR):**

```bash
yay -Syu linux-soundboard-git
```

**AppImage:**
Download the new version and replace the old one.

**New Users:**
Check the [README](../README.md) for installation instructions for your distribution.

---

## Upcoming Features

### Planned for v1.2.0

- [ ] Flathub submission and availability
- [ ] Additional audio format support
- [ ] Sound effects (pitch, speed, reverb)
- [ ] Playlist/queue system
- [ ] Keyboard shortcuts customization UI
- [ ] Sound search and filtering
- [ ] Import/export sound library

### Under Consideration

- [ ] JACK audio support
- [ ] Network streaming
- [ ] Mobile app for remote control
- [ ] Plugin system
- [ ] Scripting support (Lua/Python)
- [ ] Sound recording

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on how to contribute to this project.

---

## Links

- **GitHub Repository**: https://github.com/germanua/Linux-SoundBoard
- **Issue Tracker**: https://github.com/germanua/Linux-SoundBoard/issues
- **Discussions**: https://github.com/germanua/Linux-SoundBoard/discussions
- **AUR Package**: https://aur.archlinux.org/packages/linux-soundboard-git

---

**Note:** Dates are in YYYY-MM-DD format (ISO 8601).
