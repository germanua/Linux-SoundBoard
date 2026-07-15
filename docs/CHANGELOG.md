# Changelog

All notable changes to Linux Soundboard are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and the project versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [2.1.1] - 2026-07-15

### Changed

- **Installed-engine handoff:** Stable installations now require protocol, configuration schema, and application version equality. A stale engine is stopped, the user unit is reloaded and restarted once, and the GUI connects only after the replacement proves compatible.
- **AppImage startup:** A direct AppImage now asks whether to install for a persistent virtual microphone, run temporarily, or exit before it changes services, configuration, or the audio graph.

### Fixed

- **Microphone state after upgrades:** The installed engine and virtual microphone now survive GUI closure. Temporary engines synchronously restore the previous eligible microphone, or the existing ranked hardware/enhancement fallback, before unloading their virtual source.
- **Configuration upgrade safety:** Loading a valid schema-6 configuration creates an exact `0600` `config.json.pre-v6-backup`. A conflicting backup, malformed JSON, or future schema stops GUI startup without replacing the configuration or starting audio.
- **Engine diagnostics:** Compatibility output includes the expected and running application versions so stale package processes are visible.

## [2.1.0] - 2026-07-12

### Added

- **Delete-key removal:** Pressing Delete removes selected sounds or a focused custom tab through the same confirmation workflow as its context menu.
- **Folder-derived tabs:** Refreshing configured sound folders now creates stable tabs for immediate subfolders and assigns deeper files to their top-level folder tab.

### Changed

- **Sound removal wording:** Soundboard actions now say “Remove” instead of “Delete” and clarify that source audio files remain on disk.
- **Atomic folder refresh:** Folder scans now preserve root/subfolder identity, reconcile generated tabs and memberships in one saved configuration update, and avoid duplicate imports from overlapping roots.
- **Configuration schema v7:** Existing tabs migrate as manual tabs; generated tabs store a separate folder binding without changing editable names or existing memberships.

### Fixed

- **Mixed-version audio startup:** The UI no longer restarts a known-incompatible systemd engine and then creates a competing in-process engine. Local fallback now first stops the service and any remaining engine process, preserving single ownership of the virtual microphone.
- **Engine config safety:** The service now fails closed when it cannot load the saved configuration instead of running with defaults that disagree with the UI.
- **Audio diagnostics:** `--diagnose` now reports the UI binary, engine binary, protocol, config schema, systemd unit path, and repair command when versions are incompatible.
- **Libadwaita warnings:** The responsive breakpoint container now declares its minimum size, and startup no longer reads or writes the unsupported GTK dark-theme property.
- **Folder overlap handling:** Configured parent folders and symbolic-link aliases now have one deterministic scan owner, preventing duplicate imports and derived-tab memberships.
- **Responsive removal:** Sound, tab, and folder removal now run outside the GTK main thread; soundboard state is saved before best-effort hotkey cleanup.
- **Configuration durability:** Saved configuration files now synchronize the replacement file and its containing directory before reporting success.
- **Ogg Vorbis playback:** libvorbis-encoded Ogg files no longer stop before playback when the decoder emits an empty priming packet. Playback now advances to the first packet containing PCM audio, including after seeking.
- **Mic auto-detect selecting a screenshare source:** `Auto-detect (Default)` no longer picks a screenshare or other virtual source — Vencord/Discord screenshare, OBS virtual audio, loopback/virtual cables — as the microphone.  Auto-detect now only selects a recognised mic-enhancement chain (EasyEffects/NoiseTorch/RNNoise, preferred) or a real hardware microphone, and ignores everything else.  Hardware microphones are now detected via the PipeWire `device.id` the registry actually reports (previous builds keyed off `device.api`, which is not present at the registry layer, so hardware mics were misclassified and lost to any virtual source).  Explicit selection of any source from the dropdown is unchanged.

## [2.0.0] - 2026-05-09

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
