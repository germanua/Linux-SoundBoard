# Changelog

All notable changes to Linux Soundboard are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and the project versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
