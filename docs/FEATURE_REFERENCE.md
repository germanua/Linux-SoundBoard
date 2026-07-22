# Feature Reference

> This guide documents all current user-facing features in **Linux Soundboard** — what each function does, how to trigger it, and any important side effects.
>
> **Scope:** Covers visible app features in the current UI. Behaviors inherited from GTK rather than app-specific code are marked _GTK convention_.

---

## Quick Access

| Area                             | Location                                |
| -------------------------------- | --------------------------------------- |
| Playback & routing controls      | Top transport bar                       |
| Sound actions                    | Sound list · per-sound right-click menu |
| Tab management                   | Left sidebar                            |
| Library setup & advanced options | `Settings`                              |
| Global control hotkeys           | `Settings` → `Control Hotkeys`          |

---

## Main Window

### Sound List

The sound list is the primary library view on the right side of the window. It displays an unlabeled row-number column plus `NAME`, `DURATION`, and `HOTKEY`.

**What you can do:**

- Browse and filter sounds by the active tab or search box
- Select one or more sounds for tab operations, drag-and-drop, or bulk removal

**Interactions:**

| Action                    | How                                                         |
| ------------------------- | ----------------------------------------------------------- |
| Select a sound            | Click a row                                                 |
| Play a sound              | Activate a row (double-click, or `Enter` on selected row) ¹ |
| Multi-select (range)      | Drag across rows to rubber-band select ²                    |
| Multi-select (individual) | `Ctrl` / `Shift` + click ²                                  |

> ¹ _GTK convention_ — the app uses GTK row activation.
> ² _GTK convention_ — inherited GTK list behavior, not a custom shortcut layer.

**When you activate a sound:**

1. Any existing playback stops immediately
2. The selected sound begins playing
3. The transport bar updates to show the active sound, position, and duration
4. If the file is missing, a **recovery dialog** opens instead of playing

---

### Missing File Recovery

Appears automatically when you activate a sound whose file no longer exists.

| Button         | What it does                                                         |
| -------------- | -------------------------------------------------------------------- |
| `Locate File…` | Opens a file picker — updates the stored path and refreshes the list |
| `Remove Sound` | Removes the sound from the library and unregisters its hotkey        |
| `Cancel`       | Closes the dialog with no changes                                    |

---

## Transport Bar

The transport bar runs across the top of the main window.

---

### Play / Pause

- **Trigger:** Click the play/pause button, or use the `Play / Pause` control hotkey
- **What it does:** Pauses or resumes the currently active sound

> **Note:** The button is disabled when nothing is active. It only controls the current active track.

---

### Stop All

- **Trigger:** Click `Stop All`, or use the `Stop All` control hotkey
- **What it does:** Stops all current playback immediately

> **Note:** In `Continue` play mode, `Stop All` also suppresses automatic continuation for the stopped playback.

---

### Previous Sound

- **Trigger:** Click `Previous Sound`, or use the matching control hotkey
- **What it does:** Stops current playback and starts the previous sound in the navigation list

> **Note:** The navigation list is built from the _visible_ sound list — it follows the active tab and current search filter.

---

### Next Sound

- **Trigger:** Click `Next Sound`, or use the matching control hotkey
- **What it does:** Stops current playback and starts the next sound in the navigation list

> **Note:** Follows the active tab and current search filter, same as `Previous Sound`.

---

### Timeline Scrubber

- **Mouse:** Drag the scrubber while a sound is playing
- **Keyboard:** Focus the scrubber, then use `←` `→` `Page Up` `Page Down` `Home` `End` (and numpad equivalents)
- **What it does:** Seeks within the currently active sound

> **Notes:**
>
> - `Escape` cancels an in-progress scrub interaction
> - Disabled when nothing is playing
> - The current time label updates live while scrubbing

---

### Headphones Volume

- **Trigger:** Drag the headphones slider
- **Precise input:** Click the numeric readout → type a value (`0`–`100`) → press `Enter` or click away
- **What it does:** Sets local playback volume for your speakers or headphones

> **Notes:**
>
> - `Escape` cancels typed volume editing
> - If headphone output is muted, changes still update the saved setting

---

### Microphone Volume

- **Trigger:** Drag the microphone slider
- **Precise input:** Click the numeric readout → type a value (`0`–`100`) → press `Enter` or click away
- **What it does:** Sets how loudly the soundboard feeds the virtual microphone path

> **Note:** `Escape` cancels typed volume editing.

---

### Toggle Headphone Output

- **Trigger:** Click the headphone toggle button, or use the `Mute Headphones` control hotkey
- **What it does:** Mutes or unmutes local playback through your speakers or headphones

> **Note:** This does **not** remove sound from the virtual microphone path.

---

### Toggle Mic Passthrough

- **Trigger:** Click the microphone toggle button, or use the `Mute Real Mic` control hotkey
- **What it does:** Enables or disables real-microphone passthrough into the virtual microphone

> **Notes:**
>
> - Controls whether your real mic is mixed into `Linux_Soundboard_Mic`
> - The microphone source is configured in `Settings` → `General` → `Microphone Source`

---

### Play Mode

- **Trigger:** Click the play mode button, or use the `Cycle Play Mode` control hotkey
- **What it does:** Cycles through the three play modes below

| Mode       | Behavior                                                                                  |
| ---------- | ----------------------------------------------------------------------------------------- |
| `Default`  | Plays the selected sound once, then stops                                                 |
| `Loop`     | Loops the active sound indefinitely                                                       |
| `Continue` | When a sound finishes, automatically starts the next sound in the visible navigation list |

> **Notes:**
>
> - `Continue` follows the active tab and search filter
> - Pressing `Stop All` prevents the just-stopped playback from auto-continuing

---

### Refresh Sounds

- **Trigger:** Click the refresh button
- **What it does:**
  - Rescans all configured sound folders
  - Adds newly discovered supported audio files
  - Removes sounds whose stored file path no longer exists
  - Creates and updates tabs from immediate subfolders
  - Refreshes the library and tab counts

> **Note:** A toast notification appears when the refresh completes.

---

### Search Sounds

- **Trigger:** Type into the search box
- **What it does:** Filters the visible sound list by sound name

> **Notes:**
>
> - Case-insensitive
> - Filters within the currently selected tab
> - Previous/next navigation and `Continue` mode use the filtered list

---

### Open Settings

- **Trigger:** Click the settings button
- **What it does:** Opens the settings dialog

> **Note:** If the dialog is already open, the existing window is reused rather than creating a duplicate.

---

## Sound Actions

### Right-Click Sound Menu

Right-click any row in the sound list to open the context menu.

**Selection behavior:**

- Clicked sound **is** part of a multi-selection → actions apply to the **whole selection**
- Clicked sound **is not** part of the selection → actions apply to the **clicked sound only**

---

#### Rename

- **Trigger:** Right-click → `Rename`
- **What it does:** Opens a rename dialog and updates the sound name in the library

---

#### Set Hotkey / Update Hotkey

- **Trigger:** Right-click → `Set Hotkey` or `Update Hotkey`
- **What it does:** Opens the hotkey capture dialog for that sound

**In the dialog:**

1. Press the key combination you want
2. Click `Save` to assign it, or `Clear` to remove the existing hotkey

**Result:** The captured hotkey is bound to that sound and plays it globally when the hotkey backend is available.

> **Note:** Unsupported shortcuts are rejected by the active hotkey backend.

---

#### Check File Path

- **Trigger:** Right-click → `Check file path`
- **What it does:** Opens a dialog showing the current stored file path
- **Extra:** `Copy to Clipboard` button copies the path text

---

#### Add to Tab

- **Trigger:** Right-click → `Add to Tab` → choose a custom tab
- **What it does:** Adds the selected sound(s) to that custom tab

> **Note:** Does not remove sounds from any other tab. `General` is the full library and is not listed as an add target.

---

#### Remove from Tab

- **Trigger:** Open a custom tab → right-click a sound → `Remove from Tab`
- **What it does:** Removes the selected sound(s) from the currently open custom tab

> **Note:** Only removes tab membership — sounds remain in the main library.

---

#### Refine Loudness

- **Trigger:** Right-click → `Refine Loudness Now` (single) or `Refine Loudness (Selected)` (multi-selection)
- **What it does:** Re-analyzes and updates the LUFS loudness data for the selected sound(s)

> **Note:** Only relevant when auto-gain normalization is enabled.

---

#### Remove / Remove Selected

- **Trigger:** Right-click → `Remove` (single), `Remove Selected` (multi-selection), or press `Delete`
- **What it does:** Removes sound(s) from the library, removes all tab memberships, and unregisters associated hotkeys

> **Note:** Source audio files remain on disk. A confirmation dialog appears by default; disable it in `Settings` → `General` → `Never Ask to Confirm Removal`.

---

### Drag Sounds Between Tabs

**How:** Select one or more sounds → drag them onto a tab in the left sidebar.

| Drag direction                    | Result                                     |
| --------------------------------- | ------------------------------------------ |
| `General` → custom tab            | Adds dragged sounds to that tab            |
| Custom tab → `General`            | Removes dragged sounds from the source tab |
| Custom tab → different custom tab | Moves sounds from source tab to target tab |
| Same tab → same tab               | No change                                  |

> **Note:** A toast notification confirms a successful add, remove, or move.

---

## Tabs Sidebar

The left sidebar contains the `General` tab and all custom tabs.

---

### Select Tab

- **Trigger:** Click a tab row
- **What it does:** Filters the sound list to show only sounds in that tab

| Tab type   | Shows                            |
| ---------- | -------------------------------- |
| `General`  | Full library                     |
| Custom tab | Only sounds assigned to that tab |

Tabs generated from sound-folder subdirectories behave like custom tabs and can
be renamed. Their folder association remains stable across refreshes.

---

### Create New Tab

- **Trigger:** Click the `New Tab` button at the top of the sidebar
- **What it does:** Opens a naming dialog and creates a new custom tab

> **Note:** Empty names are rejected.

---

### Rename Tab

- **Trigger:** Right-click a custom tab → `Rename Tab`
- **What it does:** Opens a rename dialog and updates the tab name

> **Note:** `General` cannot be renamed.

---

### Delete Tab

- **Trigger:** Right-click a custom tab → `Delete Tab`, or focus it and press `Delete`
- **What it does:** Deletes the tab after confirmation

> **Notes:**
>
> - Sounds themselves are **not** deleted
> - After deletion, the app returns to `General`
> - `General` cannot be deleted

---

## Library Import and Sync

### Supported Audio Formats

| Extension(s)    | Audio format                                    |
| --------------- | ----------------------------------------------- |
| `.mp3`          | MP3                                             |
| `.ogg`          | Ogg Vorbis or mono/stereo Ogg Opus              |
| `.opus`         | Mono/stereo Ogg Opus                            |
| `.flac`         | FLAC                                            |
| `.aac`          | AAC-LC/ADTS                                     |
| `.m4a`          | AAC-LC or ALAC                                  |
| `.mp4`          | AAC-LC, ALAC, or mono/stereo Opus audio track   |

Every listed format supports playback, seeking, looping, duration, and static or dynamic LUFS normalization. The app identifies Vorbis and Opus `.ogg` files by their contents and selects the supported audio track from an MP4 file. WebM, HE-AAC, and multichannel Opus are not supported.

---

### Add Folder

- **Trigger:** `Settings` → `General` → `Sound Folders` → `Add Folder…`
- **What it does:** Adds the folder to the scan list, then immediately refreshes and imports all supported audio files found inside

> **Note:** Each immediate subfolder gets a tab. Files in deeper descendants use their top-level subfolder tab; files directly in the configured root remain in `General`. If configured folders overlap, the outer folder owns the scan; aliases and nested roots do not create duplicate sounds or tabs.

---

### Remove Folder

- **Trigger:** `Settings` → `General` → `Sound Folders` → remove button beside a folder
- **What it does:** Removes the folder, its imported sounds, and its generated tabs from the soundboard

> **Note:** Source audio files remain on disk. A generated tab containing unrelated manually added sounds is retained as a normal custom tab.

---

### Drag and Drop Audio Files

- **Trigger:** Drag supported audio files into the main window or directly onto the sound list
- **What it does:** Imports files into the library; if a custom tab is active, also adds them to that tab

> **Notes:**
>
> - Unsupported file types are skipped
> - Paths already in the library are skipped (no duplicates)
> - A drop overlay and toast feedback appear during import

---

## Settings

Open via the settings button in the transport bar.

---

### General → Playback

#### Auto-Gain Normalization

- **Path:** `Settings` → `General` → `Playback` → `Auto-Gain Normalization`
- **What it does:** Enables or disables loudness normalization across sounds

> **Note:** Enabling this may trigger background loudness analysis for sounds that lack LUFS data.

---

#### Never Ask to Confirm Removal

- **Path:** `Settings` → `General` → `Playback` → `Never Ask to Confirm Removal`
- **What it does:** Skips the confirmation dialog when removing sounds from the soundboard

---

### General → Auto-Gain Normalization

_These controls appear only when auto-gain is enabled._

| Setting                  | What it does                                                                                             |
| ------------------------ | -------------------------------------------------------------------------------------------------------- |
| **Target Volume (LUFS)** | Sets the loudness target used by normalization                                                           |
| **Auto-Gain Mode**       | `Dynamic` (default) — applies look-ahead gain shaping; `Static` — uses precomputed loudness values        |
| **Apply To**             | `Mic only (recommended)` or `Mic + headphones`                                                           |
| **Look-ahead (ms)**      | _(Dynamic only)_ Anticipation window for gain changes                                                    |
| **Attack (ms)**          | _(Dynamic only)_ How quickly gain reductions are applied                                                 |
| **Release (ms)**         | _(Dynamic only)_ How quickly gain returns to normal                                                      |

#### Analyze All Sounds

- **Trigger:** Click `Analyze`
- **What it does:** Scans sounds that do not yet have loudness data

> **Note:** The spinner remains active while the Pending, Estimated, Refined, and Unavailable counts update after each completed sound. Click `Stop` to cancel the remaining work.

#### Refine Estimated Sounds

- **Trigger:** Click `Refine`
- **What it does:** Runs full loudness analysis for sounds that currently use an estimate

> **Note:** The same live status counts and `Stop` control are available while refinement runs.

---

### General → Microphone Routing

- **Trigger:** Choose a routing mode from the dropdown
- **Options:**
  - `Default (recommended)` — the soundboard claims the system default mic so recording apps use it automatically
  - `Manual` — the soundboard never changes the system default
- **What it does:** Controls whether the soundboard sets itself as the default audio input device

> **Note:** Switching modes takes effect immediately. A confirmation dialog may appear when changing to Default mode.

---

### General → Microphone Source

- **Trigger:** Choose a source from the dropdown
- **Options:** `Auto-detect (Default)` or any enumerated PipeWire source the app can see
- **What it does:** Selects which real microphone is used for mic passthrough

> **Note:** If mic passthrough is already active, changing the source restarts it with the new source.

> **Auto-detect behavior:** `Auto-detect (Default)` only ever auto-selects two kinds of source: a recognised mic-enhancement chain (EasyEffects, NoiseTorch, RNNoise — preferred, since you deployed it to process your mic) or a real hardware microphone. It never auto-selects other virtual sources — screenshare audio such as Vencord/Discord "Share Sound", OBS virtual audio, loopback/virtual cables, or unnamed custom virtual sources — because those carry application audio, not your voice. Any of those still appears in the dropdown and can be selected explicitly if you really want it.

---

### General → Passthrough Status

- **Display:** Shows the current state of mic passthrough
- **Values:** `Active: [microphone name]` when passthrough is running, or `Waiting for microphone…` when the selected source is not yet available

> **Note:** This is a read-only status indicator, not a control.

---

### General → Mic Latency Profile

- **Trigger:** Choose a profile from the dropdown
- **Options:**
  - `Balanced (recommended)` — stable default for most systems
  - `Low latency` — lower queueing delay with minimal extra CPU
  - `Ultra latency (experimental)` — lowest possible delay, may increase CPU usage
- **What it does:** Adjusts the internal buffer and queueing parameters for mic passthrough

---

### General → Appearance

| Setting        | Options            | Effect                                                              |
| -------------- | ------------------ | ------------------------------------------------------------------- |
| **Theme**      | `Dark` / `Light`   | Changes the app theme immediately and saves the preference          |
| **List Style** | `Compact` / `Card` | `Compact` — dense list, more sounds visible; `Card` — balanced layout with about 1.6x the space of compact |

---

### General → About

Displays the app name, current version, and supported audio formats. The format
list is `WAV, MP3, OGG, OPUS, FLAC, M4A, AAC, MP4`. M4A supports AAC-LC or ALAC;
MP4 supports AAC-LC, ALAC, or mono/stereo Opus audio tracks.

---

## Global Control Hotkeys

Open via `Settings` → `Control Hotkeys`.

**Each hotkey row has two controls:**

| Control  | How to use                                                                                   |
| -------- | -------------------------------------------------------------------------------------------- |
| `Record` | Click `Record` → click the capture area if needed → press the key combination → click `Save` |
| `Clear`  | Removes the assigned hotkey immediately                                                      |

> **Capture notes:**
>
> - Unsupported key combinations are rejected by the active backend
> - `Escape` cancels the current capture attempt inside the dialog

---

### Available Global Hotkeys

| Hotkey              | What it does                                                    |
| ------------------- | --------------------------------------------------------------- |
| **Play / Pause**    | Toggles playback of the active sound                            |
| **Stop All**        | Stops all currently playing sounds                              |
| **Previous Sound**  | Plays the previous sound in the current visible navigation list |
| **Next Sound**      | Plays the next sound in the current visible navigation list     |
| **Mute Headphones** | Toggles local headphone/speaker output                          |
| **Mute Real Mic**   | Toggles real microphone passthrough into the virtual microphone |
| **Cycle Play Mode** | Cycles `Default` → `Loop` → `Continue` → `Default`              |

---

## Status Banners and Feedback

### PipeWire Unavailable Banner

- **When:** Startup detects PipeWire is unavailable
- **Meaning:** The virtual microphone path is not available
- **Action:** `Dismiss`

---

### Hotkeys Unavailable Banner

- **When:** Startup detects the global hotkey backend is unavailable
- **Meaning:** Global hotkeys cannot be used until the backend issue is resolved
- **Action:** `Dismiss`

---

### Toast Notifications

Short toasts appear for:

- Sound refresh completion
- Drag-and-drop file imports
- Sound-to-tab drag-and-drop actions

---

## External Audio Routing

### Use the Virtual Microphone in Other Apps

- **How:** In Discord, OBS, Zoom, or another app, set the audio **input device** to `Linux_Soundboard_Mic`
- **What it does:** Routes Linux Soundboard output into that application as a microphone source

> **Note:** To also include your real microphone, enable mic passthrough in the transport bar (see [Toggle Mic Passthrough](#toggle-mic-passthrough)).
