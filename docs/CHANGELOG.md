# Changelog

All notable changes to Linux Soundboard are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and the project versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `PlayReplace` IPC request to the audio engine protocol.  Stop-all and play-new are now a single atomic engine operation, eliminating a race condition where the snapshot poller could observe the transient "all stopped" state between the two calls.

### Fixed

- **Continue play mode:** clicking a sound while Continue mode is active no longer causes the app to advance to the next sound instead of replaying the clicked one.  The fix uses a UI-side pending-play flag that prevents the Continue auto-advance from firing on the transient empty snapshot that occurs between `stop_all` and `play` on the worker thread.
- **Stop on close:** closing the UI window now sends a `StopAll` command to the audio engine before disconnecting, so any actively playing sounds stop immediately instead of continuing to play after the window is dismissed.
- **Headphones mute button:** the button now shows exactly two states (headphones on / headphones with a slash) instead of cycling through three icons.  The initialization path was using the wrong icon constants (`LOCAL_AUDIO` instead of `HEADPHONES`) so the first click produced an unexpected icon.
- **Headphones icons at small size:** the headphone SVG icons were redesigned from 24×24 stroke-based paths (which rendered at sub-pixel widths at button scale) to 16×16 fill-based paths that match the microphone icon style and remain sharp at any button size.
- **swhkd hotkey format:** the `~` (don't-swallow / pass-through) prefix is now placed before the final key token only (`ctrl + ~l`) instead of before the entire combination (`~ctrl + l`).  swhkd 1.3.0-dev rejects the latter form and was logging "expected command" for every registered hotkey.

## [1.1.2] - 2026-04-01

### Fixed

- Native packages and the AppImage now install a launcher icon name that desktop search menus resolve consistently.
- RPM packaging now refreshes icon and desktop caches after install and uninstall so the app appears in search without manual cache rebuilds.

## [1.1.1] - 2026-04-01

### Added

- Explicit `LSB_FORCE_X11=1` startup override support for native builds.
- README acknowledgments and a dedicated `THIRDPARTY_LICENSES.md` notice file for major third-party components and licenses.

### Changed

- VMware guests now prefer a safer GTK renderer path automatically when `GSK_RENDERER` is not already set.
- Troubleshooting documentation now separates renderer issues, session backend issues, and package-install issues more clearly.
- Release metadata, package examples, and downstream packaging files were synced for the 1.1.1 release.

## [1.1.0] - 2026-03-24

### Added

- Native Wayland support with `swhkd` for global hotkeys.
- Native X11 hotkey backend for X11 and XWayland sessions.
- Official Debian and RPM packaging workflows.
- Flatpak packaging files and maintainer workflow.
- Bootstrap installer script for distro-aware setup.
- Release automation around checksums and GitHub release assets.

### Changed

- Distribution support and installation guidance were expanded beyond Arch and AppImage-only distribution.
- Packaging layout was split into dedicated Debian, RPM, Flatpak, Linux bundle, and AUR paths.
- Documentation was reorganized around install, troubleshooting, contributing, and packaging workflows.

### Fixed

- Virtual microphone creation issues on modern PipeWire-based systems.
- AppImage backend handling for Wayland-capable environments.
- Hotkey behavior across Wayland and X11 packaging targets.

## [1.0.0] - 2026-03-22

### Added

- Initial public release.
- Virtual microphone routing for Discord, OBS, Zoom, and similar applications.
- Mic passthrough, loudness normalization, folder sync, drag and drop, and global hotkeys.
- GTK4 and Libadwaita desktop UI with dark and light theme support.
- AUR package and AppImage distribution.
