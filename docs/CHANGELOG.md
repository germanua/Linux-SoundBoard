# Changelog

All notable changes to Linux Soundboard are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and the project versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Installer menu:** The one-line install command now opens a menu to install the newest version, install a previous release, uninstall, fix setup problems, generate a bug report, or show status. Every action is also a command (`install --version`, `versions`, `fix`, `report`), and piping the script with no arguments still installs directly. Each entry states whether it needs your password, based on the distro, session type, and whether a native package is installed.
- **Previous releases:** Any published release can be installed from its tarball into `~/.local` without root. A native package that would shadow it is removed first, with confirmation.
- **Audio snapshots:** Installs and updates record the default microphone and speakers, the engine service state, and checksums of the PipeWire, WirePlumber, and PulseAudio configuration before making changes. Uninstall prints what changed since that snapshot and asks once whether to restore it. Inspect it any time with `install-user.sh snapshot-diff`.
- **Bug reports:** `install.sh report` writes a single file with a system report, an application report, and a blank for the user to fill in, then explains how to attach screenshots to a GitHub issue. The home path and username are replaced; sound-device names are kept.

### Fixed

- **Installer prompts through the one-liner:** Prompts read from the terminal instead of the piped script, so the installer can ask questions when started with `curl ... | bash`. Previously every prompt silently took its non-interactive default.
- **Uninstall leftovers:** Removing the user installation now also removes the licence and notice files it deployed, so `~/.local/opt/linux-soundboard/` no longer survives an uninstall.
- **Default microphone after a PipeWire or WirePlumber restart:** The engine forgets what it believed about the system default whenever the metadata object carrying it is replaced or goes away, and re-evaluates. A replaced object reports no properties, so an engine that kept its previous belief would skip the reclaim and go silent while the system had no default at all. The change is also logged, because the failure it caused left no trace.
- **Default microphone after an engine restart:** In Default routing mode the engine reclaims the virtual microphone when the system default is cleared, not only when another device is selected. Restarting the engine — which every update does — replaces the virtual microphone node and clears the default, so the soundboard could silently stop being the default input until something else set one. Reclaiming a cleared default now writes the default source directly instead of asking WirePlumber to switch to the device it already had configured, which changed nothing and left the system on a fallback microphone.

## [2.2.0] - 2026-07-28

### Added

- **SQLite sound library:** Sounds, the folder tree, tab membership, and hotkey bindings are stored in `~/.config/linux-soundboard/library.sqlite3`. `config.json` keeps settings only. An existing configuration is migrated on first launch and the original is preserved as `config.json.pre-v8-backup`. If the library cannot be opened, startup offers to restore that backup and archives the current files rather than deleting them.
- **Nested folder navigation:** The sidebar shows the complete folder hierarchy instead of only immediate subfolders. Child folders load on demand as folders are expanded, and a parent folder lists the sounds of everything beneath it.
- **Remove a folder from the sidebar:** Right-click a folder → `Remove Folder` hides that folder and its subtree. The confirmation states how many sounds stop appearing. Nothing is deleted on disk and a rescan does not bring it back.
- **Restore removed folders:** `Settings` → `General` → `Removed Folders` lists hidden folders with a `Restore` button. The group appears only while something is hidden.
- **Reorder folders:** Dragging a folder into the gap above or below a sibling changes the order, which is saved per folder.
- **Combine folders:** Dropping a folder onto another folder's row offers to move every sound it resolves to into that folder. Files are not moved or renamed, and a folder cannot be combined into its own subtree.
- **Folder membership overrides:** Sounds can be dragged onto a folder to include them, excluded with `Exclude from Folder`, and reset with `Restore Natural Membership`.
- **Cancellable folder scans:** The scan that follows `Add Folder…` can be stopped from the folder row or the `Stop` button beside it.

### Changed

- **Library size limits:** Queries, scans, imports, and tab updates run in bounded pages on a background worker, so window size and memory no longer track the size of the library.
- **Settings and dialogs:** Pressing outside the settings panel or a dialog closes it, and dialogs stay above the settings panel. The settings panel is built on first use.
- **Loudness status:** Refinement publishes progress while a run is going, counts refresh after a scan, and Analyze and Refine no longer cancel each other.
- **Memory diagnostics:** Library figures are read from the store and refreshed as the library changes instead of being estimated from configuration arrays.

### Fixed

- **Folder scanning coverage:** Folders holding only WAV files, folders with more sounds than a single scan batch, and deeper subfolder levels are imported completely.
- **Sidebar stability under scrolling:** Scrolling back to a released folder page no longer crashes, folder pages requested during fast scrolling no longer stay blank, and expansion state survives a folder-tree rebuild.
- **Auto-gain backfill:** Enabling Auto-Gain Normalization starts the missing-loudness analysis again instead of finding nothing to do.
- **Transport track name:** The playing sound's name is shown again.
- **Engine shutdown:** A stale engine's shutdown request can no longer stop the current persistent engine.
- **Settings appearance:** Settings rows no longer draw a focus ring inside a focus ring, and sidebar resize no longer renders incorrectly.

### Performance

- **Startup:** Configuration load, library open, the first sound page, player setup, and the PipeWire probe run off the GTK thread. On a 156,000-sound library the first rows appear in 31-35 ms instead of 105-109 ms.
- **Wide folder trees:** Sidebar rows are virtualized, the folder list keeps a fixed number of pages materialized, and retained child rows are capped. On a 20,000-folder library a deep folder page loads in 11.9 ms instead of 17.3 s and idle memory is lower.

## [2.1.2] - 2026-07-19

### Added

- **Ogg Opus support:** Mono and stereo Ogg Opus files can be imported as `.opus` or `.ogg` and use playback, seeking, looping, exact duration, and static or dynamic LUFS normalization on local and virtual-microphone outputs. Ogg Vorbis `.ogg` files remain supported; WebM and multichannel Opus are not supported.

### Changed

- **Auto-gain defaults:** New configurations use Dynamic auto-gain by default. Existing saved Static or Dynamic choices are preserved.
- **Loudness analysis status:** Analyze and Refine now update the Pending, Estimated, Refined, and Unavailable counts as each sound completes.
- **About details:** Settings now shows the current application version and supported audio formats.
- **Native package support:** Arch, Debian, RPM, and AppImage packaging includes the Opus runtime required for Ogg Opus playback.

### Fixed

- **M4A and MP4 playback:** ALAC M4A and supported MP4 audio tracks now play, seek, report duration, and use static or dynamic LUFS normalization instead of failing decoder creation.
- **Ogg Opus playback:** Header gain, pre-skip, end trimming, seeking, looping, and duration now follow the Ogg Opus stream metadata, including malformed or truncated stream rejection.
- **Dynamic normalization:** Dynamic mode can reach louder LUFS targets while its limiter controls output peaks.
- **Audio stream endings:** Sample-rate conversion no longer drops the final converted frame for Ogg Opus or Ogg Vorbis playback.
- **Folder-derived tabs:** Empty subfolders and folders containing only unsupported files no longer appear in the sidebar.
- **Loudness analysis recovery:** Analyze and Refine recover after Stop, and missing or terminally invalid sounds no longer remain indefinitely Pending or Estimated.
- **Analysis activity indicators:** Analyze and Refine spinners continue rotating while status counts refresh instead of restarting their animation.

### Security

- **Dependency soundness:** Updated the locked `anyhow` dependency to 1.0.103, which fixes RUSTSEC-2026-0190.
- **Release workflow:** GitHub Actions are pinned to immutable commits and use read-only repository permissions.

## [2.1.1] - 2026-07-16

### Changed

- **Arch installation:** The recommended installer and documentation now use the tagged `linux-soundboard` AUR package. `linux-soundboard-git` remains available for development testing.
- **Installed-engine handoff:** Stable installations now require protocol, configuration schema, and application version equality. A stale engine is stopped, the user unit is reloaded and restarted once, and the GUI connects only after the replacement proves compatible.
- **AppImage startup and updates:** A first direct AppImage launch asks whether to install for a persistent virtual microphone, run temporarily, or exit. Once installed, opening a newer downloaded AppImage automatically updates the stable user copy and restarts the matching engine; version tracking prevents silent downgrades.

### Fixed

- **AUR release binaries:** Release builds no longer embed the temporary source-tree fallback path for the bundled swhkd installer helper.
- **Arch upgrades:** The recommended installer now replaces an installed `linux-soundboard-git` package with the stable `linux-soundboard` package non-interactively for both AUR-helper and manual-build paths.
- **Microphone state after upgrades:** The installed engine and virtual microphone now survive GUI closure. Temporary engines synchronously restore the previous eligible microphone, or the existing ranked hardware/enhancement fallback, before unloading their virtual source.
- **Configuration upgrade safety:** Loading a valid schema-6 configuration creates an exact `0600` `config.json.pre-v6-backup`. A conflicting backup, malformed JSON, or future schema stops GUI startup without replacing the configuration or starting audio.
- **Engine diagnostics:** Compatibility output includes the expected and running application versions so stale package processes are visible.
- **Engine update feedback:** The GUI reports successful stale-engine replacement. A failed replacement explains the temporary fallback and opens the documented recovery steps directly.
- **Native package builds:** Debian and RPM recipes now declare the PipeWire and Clang build dependencies required by clean build environments.

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
