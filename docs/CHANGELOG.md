# Changelog

All notable changes to Linux Soundboard are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and the project versioning follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- **The playing row lost its highlight on every second line:** The alternating row shading and the playing/active row background both target the cell node with the same CSS specificity, so the one declared later won — and that was the shading. A sound on an even row kept its left accent bar but was painted the same colour as an idle row, which made the highlight look like it appeared only half the time. The shading now skips cells that carry a playing or active state, so the highlight no longer depends on where the sound sits in the list.
- **AppImage showed the desktop's icons instead of its own:** The AppImage generates a hicolor `index.theme` for its bundled application icons, and `AppRun` puts that directory ahead of the host in `XDG_DATA_DIRS`, so it becomes the hicolor index the app itself reads. GTK counts the resource paths registered by the application as part of hicolor and only scans the subdirectories the index declares; the generated file listed the `*/apps` sizes and nothing else, so every bundled symbolic icon in `scalable/actions`, `scalable/devices`, and `scalable/places` was invisible and each toolbar button silently fell back to whatever the desktop icon theme provided. The index now declares those contexts, and `packaging/smoke-check.sh` fails if a new one is added without it.

### Changed

- **Consistent, denser interface:** The stylesheets set about fifteen font sizes in absolute pixels while the labels that carry the most weight — sound names, tab names, preference row titles — set none and rendered at the raw desktop size, so a single list row could show three different sizes. Every size now sits on one scale expressed in `rem`, which also means the whole interface follows the desktop text-scaling setting; pixel font sizes did not, because GTK applies them as absolute Pango sizes. Toolbar buttons, list rows, list headers, dialogs, and the settings panel were tightened to match, and preference rows no longer keep GNOME's roomier metrics inside a window built to a denser rhythm.

## [2.3.1] - 2026-08-22

### Fixed

- **Native and tarball dependencies:** Package metadata and installers now include the PulseAudio and PipeWire command providers the app invokes, package builds use the locked Rust graph, and distro-selected Rust packages are no longer constrained by duplicated version literals. Stale ALSA build dependencies were removed.
- **Release icon churn:** Package builds now use the committed icons instead of rewriting identical PNGs and leaving the release tree dirty.

## [2.3.0] - 2026-08-22

### Added

- **System tray and background running:** `Settings` → `General` → `System Tray` puts Linux Soundboard in the status area and lets the close button hide the window instead of quitting, so global hotkeys keep working with the window shut. Left-click the icon to show or hide the window; right-click for a short menu — show/hide, play/pause, stop all, mute the real mic, and quit. Both switches are on by default, but the window is only ever hidden while an icon is really showing, so on a desktop with no tray the close button quits exactly as before. Needs a desktop that supports `StatusNotifierItem`: KDE Plasma, XFCE, LXQt, Cinnamon, MATE, Budgie and waybar do; GNOME needs the AppIndicator extension.
- **Show the playing sound in the desktop's media controls:** `System Tray` → `Show In Media Controls` publishes the current sound to the panel's media widget with working transport buttons, and on GNOME this works without any extension. Off by default: with it on the app counts as a media player, so the media keys may reach it instead of a music player. The controls stay in the panel while the switch is on, reporting `Stopped` between sounds, because a soundboard clip ends before controls that came and went with it could be pressed; switching the setting off releases them at once.
- **Bind several sounds to one hotkey:** `Settings` → `Control Hotkeys` → `Hotkey Behaviour` → `Multiple Sounds Per Hotkey` lets a shortcut hold more than one sound. A new `Shared Hotkey Mode` decides which one a press plays — replay the same sound, advance to the next, or pick at random — and can be switched from a hotkey of its own. Assigning a shortcut another sound already answers to names that sound and asks first, so a group is never formed by accident. This is separate from `Play Mode`, which still only decides what happens when a sound finishes.
- **Give each tab its own hotkey:** `Hotkey Behaviour` → `Tab Hotkeys` binds a shortcut to a tab, and while that tab is open only its sound hotkeys respond, so the same combination can mean different sounds in different tabs. Right-click a tab in the sidebar to bind it, `General` included; a tab's own hotkey works from anywhere. With the switch on, the sound hotkey dialog can limit a binding to the tab it is set from. Both switches are off by default and independent of each other, and hotkeys assigned before this release keep answering everywhere. Folder tabs cannot be bound yet.
- **Choose how to install:** `install.sh` now asks whether to install automatically, from the AppImage, from the binary tarball, or from the native package, and the same choice is available noninteractively as `install --method auto|appimage|tarball|native`. Choosing the AppImage checks for FUSE first, since an installed AppImage mounts itself at every launch. Automatic stays the default and keeps the previous behaviour, and previous-version installs accept the tarball and AppImage methods.

### Changed

- **A sound can carry a different hotkey in each tab:** with `Tab Hotkeys` on, the same sound shown in two tabs takes a separate shortcut in each, and the hotkey column shows the one that applies where you are looking — a shortcut limited to another tab is no longer displayed as if it worked here. A tab's own shortcut takes precedence over one that is live everywhere. The scope checkbox in the hotkey dialog now defaults to this tab, since that is the point of turning the setting on.
- **The library database upgrades on first launch:** storing which tab a hotkey belongs to needed a schema change. The upgrade is automatic and keeps every existing hotkey working exactly as before, but it is one-way: an older build will refuse to open the library afterwards.

### Fixed

- **Auto-gain did nothing until it was toggled off and on:** Sounds imported by drag-and-drop, folder scan, or rescan never had their loudness measured, and auto-gain applies no gain to a sound with no measurement — in Dynamic mode as well as Static. Those paths now schedule the same analysis that adding a single sound already did.
- **Wayland hotkeys on systems without the uinput module:** swhkd lists the uinput kernel module as a runtime dependency but its installer does not provide it. It needs `/dev/uinput`, whose device node exists even when the module is not loaded, so it failed with `Failed to create uinput device` while the suggested fix talked about the setuid bit. The in-app install and `install.sh` now probe for it and, only where it is genuinely missing, explain what uinput is and ask before loading it and registering it in `/etc/modules-load.d/`; systems that already provide it — the large majority, since the kernel autoloads the module when the node is opened — are left untouched and are never asked. Declining still installs swhkd. The copy-paste commands include both steps, and the error names the real remedy.
- **Tarball installs left the runtime libraries to chance:** The tarball carries only the binary, which needs GTK 4, libadwaita, PulseAudio, Opus, PipeWire, and X11 from the distro — the same set the `.deb`, `.rpm`, and AUR packages declare. `install.sh` now checks for those libraries by soname and offers to install the matching packages, staying silent (and password-free) when the system already has them. Package names that differ between releases are resolved against the package index rather than hardcoded: `pkexec` versus `policykit-1`, and `libfuse2t64` versus `libfuse2` for the AppImage method, which also pulls the `fusermount` helper the image needs to mount itself.
- **Downloads are verified:** every download — tarball, `.deb`, `.rpm`, and AppImage — is now checked against the release's `SHA256SUMS.txt`. A mismatch stops the install; a release without the list only warns, so older versions stay installable. Releases now publish that list: every packaging script refreshes `dist/SHA256SUMS.txt` through the new `packaging/generate-checksums.sh`.
- **Misleading launch hint and shadowed installs:** `install.sh` claimed the app could be launched as `linux-soundboard` after a tarball or AppImage install, but those land in `~/.local/opt` and nothing is put on `PATH`; it now prints the real path. Installing into `~/.local` while a native package is present also warns first, as pinned-version installs already did.
- **Blank hotkey status text:** The status shown in Settings and in the main-window banner is parsed as markup, so a remediation command containing `&&` aborted the parse and left the text empty.
- **AppImage theming:** The bundled GTK runtime hook no longer exports `GTK_THEME`, which overrode the libadwaita stylesheet and rendered the AppImage in the stock light theme regardless of the application's own dark/light setting. Set `LSB_GTK_THEME` to force a GTK theme by hand.
- **Missing animations on GTK before 4.20:** The playing-dot and hotkey-recording animations declared two keyframes in one selector list, which older GTK releases reject with `Theme parser error: Expected '{'`, dropping the animation. Each keyframe now has its own block.
- **Persistent audio engine after an AppImage install:** The engine user service no longer applies its sandboxing options (`NoNewPrivileges`, `RestrictSUIDSGID`, `LockPersonality`) when it starts an AppImage. Each of them implies `NoNewPrivileges` — the last two through seccomp — which blocked the setuid FUSE helper the image needs to mount itself and left the service failing with exit code 127. Native package installs keep the full hardening. The unit also points at the installed copy instead of the downloaded file, stops retrying after five failed starts in a minute, and the startup dialog now offers to run temporarily or open troubleshooting instead of only exiting.

## [2.2.1] - 2026-07-29

### Fixed

- **Auto-gain immediately after analysis:** Static and Dynamic normalization now resolve the current sound row from SQLite before playback, so loudness values written by a completed analysis take effect without restarting or refreshing the application.

## [2.2.0] - 2026-07-29

### Added

- **SQLite sound library:** Sounds, the folder tree, tab membership, and hotkey bindings are stored in `~/.config/linux-soundboard/library.sqlite3`. `config.json` keeps settings only. An existing configuration is migrated on first launch and the original is preserved as `config.json.pre-v8-backup`. If the library cannot be opened, startup offers to restore that backup and archives the current files rather than deleting them.
- **Nested folder navigation:** The sidebar shows the complete folder hierarchy instead of only immediate subfolders. Child folders load on demand as folders are expanded, and a parent folder lists the sounds of everything beneath it.
- **Remove a folder from the sidebar:** Right-click a folder → `Remove Folder` hides that folder and its subtree. The confirmation states how many sounds stop appearing. Nothing is deleted on disk and a rescan does not bring it back.
- **Restore removed folders:** `Settings` → `General` → `Removed Folders` lists hidden folders with a `Restore` button. The group appears only while something is hidden.
- **Reorder folders:** Dragging a folder into the gap above or below a sibling changes the order, which is saved per folder.
- **Combine folders:** Dropping a folder onto another folder's row offers to move every sound it resolves to into that folder. Files are not moved or renamed, and a folder cannot be combined into its own subtree.
- **Folder membership overrides:** Sounds can be dragged onto a folder to include them, excluded with `Exclude from Folder`, and reset with `Restore Natural Membership`.
- **Cancellable folder scans:** The scan that follows `Add Folder…` can be stopped from the folder row or the `Stop` button beside it.
- **Installer menu:** The one-line install command now opens a menu to install the newest version, install a previous release, uninstall, fix setup problems, generate a bug report, or show status. Every action is also a command (`install --version`, `versions`, `fix`, `report`), and piping the script with no arguments still installs directly. Each entry states whether it needs your password, based on the distro, session type, and whether a native package is installed.
- **Previous releases:** Any published release can be installed from its tarball into `~/.local` without root. A native package that would shadow it is removed first, with confirmation.
- **Audio snapshots:** Installs and updates record the default microphone and speakers, the engine service state, and checksums of the PipeWire, WirePlumber, and PulseAudio configuration before making changes. Uninstall prints what changed since that snapshot and asks once whether to restore it. Inspect it any time with `install-user.sh snapshot-diff`.
- **Bug reports:** `install.sh report` writes a single file with a system report, an application report, and a blank for the user to fill in, then explains how to attach screenshots to a GitHub issue. The home path and username are replaced; sound-device names are kept.

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
- **Default microphone after an engine restart:** In Default routing mode the engine reclaims the virtual microphone when the system default is cleared, not only when another device is selected. Restarting the engine — which every update does — replaces the virtual microphone node and clears the default, so the soundboard could silently stop being the default input until something else set one. Reclaiming a cleared default now writes the default source directly instead of asking WirePlumber to switch to the device it already had configured, which changed nothing and left the system on a fallback microphone.
- **Default microphone after a PipeWire or WirePlumber restart:** The engine forgets what it believed about the system default whenever the metadata object carrying it is replaced or goes away, and re-evaluates. A replaced object reports no properties, so an engine that kept its previous belief would skip the reclaim and go silent while the system had no default at all. The change is also logged, because the failure it caused left no trace.
- **Installer prompts through the one-liner:** Prompts read from the terminal instead of the piped script, so the installer can ask questions when started with `curl ... | bash`. Previously every prompt silently took its non-interactive default.
- **Uninstall leftovers:** Removing the user installation now also removes the licence and notice files it deployed, so `~/.local/opt/linux-soundboard/` no longer survives an uninstall.

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
