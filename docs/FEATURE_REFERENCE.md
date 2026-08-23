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
| Tab management                   | Left sidebar · `TABS`                   |
| Folder browsing & organization   | Left sidebar · `FOLDERS`                |
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
  - Updates the folder hierarchy in the sidebar, and folder tabs kept from earlier versions
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

> **Notes:**
>
> - With `Multiple Sounds Per Hotkey` on, a combination another sound already
>   answers to names that sound and asks before adding to it
> - With `Tab Hotkeys` on, the dialog also offers to limit the hotkey to the
>   tab it is set from, checked by default for a new shortcut
>
> See [Hotkey Behaviour](#hotkey-behaviour).

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

#### Exclude from Folder

- **Trigger:** Open a folder → right-click a sound → `Exclude from Folder`
- **What it does:** Hides the selected sound(s) from that folder without touching the files or any other folder

---

#### Restore Natural Membership

- **Trigger:** Right-click a sound → `Restore Natural Membership`
- **What it does:** Drops the include and exclude overrides on the selected sound(s), so the folder they appear in follows their location on disk again

---

#### Refine Loudness

- **Trigger:** Right-click → `Refine Loudness Now` (single) or `Refine Loudness (Selected)` (multi-selection)
- **What it does:** Re-analyzes and updates the LUFS loudness data for the selected sound(s)

> **Note:** Only relevant when auto-gain normalization is enabled.

---

#### Remove / Remove Selected

- **Trigger:** Right-click → `Remove` (single), `Remove Selected` (multi-selection), or press `Delete`
- **What it does:** Removes sound(s) from the library, removes all tab memberships, and unregisters associated hotkeys

> **Note:** Source audio files remain on disk. A confirmation dialog appears by default; disable it in `Settings` → `General` → `Behavior` → `Never Ask to Confirm Removal`.

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

### Drag Sounds Into a Folder

**How:** Select one or more sounds → drag them onto a folder row in the left sidebar.

- **What it does:** Makes the dragged sounds appear in that folder in addition to where they already appear
- Undo it per sound with `Exclude from Folder`, or reset every override with `Restore Natural Membership`

> **Note:** This changes folder membership only. No file is moved or renamed on disk.

---

## Tabs Sidebar

The left sidebar has two sections. `TABS` holds `General` and every custom tab.
`FOLDERS` holds the scanned folders as a hierarchy.

---

### Select Tab

- **Trigger:** Click a tab row
- **What it does:** Filters the sound list to show only sounds in that tab

| Tab type   | Shows                            |
| ---------- | -------------------------------- |
| `General`  | Full library                     |
| Custom tab | Only sounds assigned to that tab |

Folder tabs generated by earlier versions are kept, behave like custom tabs, and
can be renamed. Their folder association remains stable across refreshes. New
scans no longer create them; scanned folders appear under `FOLDERS` instead.

---

### Select Folder

- **Trigger:** Click a folder row under `FOLDERS`, or click the arrow to expand it
- **What it does:** Filters the sound list to that folder, including everything in its subfolders

> **Notes:**
>
> - The full hierarchy is available; subfolders load as they are expanded
> - A folder row is not a tab: adding a sound to a tab does not change what a folder shows
> - Right-click a folder for `Rename Folder`, `Move Up`, `Move Down`, and `Remove Folder`

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

### Set Tab Hotkey

- **Trigger:** Right-click a tab → `Set Tab Hotkey`
- **What it does:** Binds a hotkey that makes that tab active

> **Notes:**
>
> - Shown only while `Tab Hotkeys` is on; see [Hotkey Behaviour](#hotkey-behaviour)
> - Works from any tab, so there is always a way back
> - `General` can be bound too; folder tabs cannot be bound yet

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

> **Notes:**
>
> - Subfolders appear under `FOLDERS` in the sidebar, at their real depth. Scanning no longer creates a tab per subfolder
> - The scan runs in the background. While it does, the row reads `Scanning… activate to cancel` and the `Stop` button beside it cancels the scan; sounds already imported are kept
> - If configured folders overlap, the outer folder owns the scan; aliases and nested roots do not create duplicate sounds

---

### Remove Folder

- **Trigger:** `Settings` → `General` → `Sound Folders` → remove button beside a folder
- **What it does:** Removes the scanned root, its imported sounds, its place in the sidebar's `FOLDERS` section, and any folder tabs kept from earlier versions

> **Notes:**
>
> - Source audio files remain on disk. A folder tab containing unrelated manually added sounds is retained as a normal custom tab
> - This is the whole scanned root, unlike `Remove Folder` in the sidebar, which hides one folder and can be undone
> - Anything hidden under that root disappears from `Removed Folders` along with it

---

### Remove a Folder from the Sidebar

- **Trigger:** Right-click a folder in the sidebar → `Remove Folder`
- **What it does:** Hides that folder and everything under it. Its sounds stop appearing in `General` and in folder views. The confirmation states how many sounds are affected.

> **Notes:**
>
> - Nothing is deleted or moved on disk, and a rescan does not bring the folder back
> - A sound that also appears in a folder you kept stays visible
> - Restore it under `Settings` → `General` → `Removed Folders`, which only appears when something is hidden
> - Removing the whole scanned root instead deletes these entries along with the folders

---

### Reorder and Combine Folders

- **Trigger:** Drag a folder row in the sidebar
- **What it does:** Dropping in the gap above or below a sibling reorders the list. Dropping on the middle of another folder offers to move that folder's sounds into it.

> **Notes:**
>
> - A line in the gap means reorder; a filled row means the sounds go into that folder
> - Reordering is limited to a folder's own siblings; ordering is saved per folder
> - Combining changes folder membership only — no file is moved or renamed
> - A folder cannot be combined into itself or into one of its own subfolders

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

### General → Removed Folders

- **Trigger:** `Settings` → `General` → `Removed Folders` → `Restore`
- **What it does:** Brings a folder removed from the sidebar back, with its sounds

> **Notes:**
>
> - The group is shown only while at least one folder is hidden
> - The list is re-read every time the settings panel is opened
> - Removing the whole scanned root under `Sound Folders` discards these entries along with the folders

---

### General → Behavior

#### Never Ask to Confirm Removal

- **Path:** `Settings` → `General` → `Behavior` → `Never Ask to Confirm Removal`
- **What it does:** Skips the confirmation dialog when removing sounds from the soundboard

---

### Audio → Playback

#### Auto-Gain Normalization

- **Path:** `Settings` → `Audio` → `Playback` → `Auto-Gain Normalization`
- **What it does:** Enables or disables loudness normalization across sounds

> **Note:** Enabling this may trigger background loudness analysis for sounds that lack LUFS data.

---

#### Loudness Boost

- **Path:** `Settings` → `Audio` → `Playback` → `Loudness Boost`
- **What it does:** Adds `0–150 dB` of gain to sounds sent to the virtual microphone

> **Notes:**
>
> - Loudness Boost works whether LUFS normalization is enabled or disabled
> - When both are enabled, normalization and its optional limiter run before the raw boost
> - Local headphones and speakers are never boosted
> - Extreme values hard-clip and distort; the value is digital gain, not physical sound-pressure level

---

### Audio → Auto-Gain Normalization

_These controls remain visible and can be configured while auto-gain is disabled._

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

### Audio → Loudness Boost

_These controls remain visible and can be configured while Loudness Boost is disabled._

| Setting        | What it does                                                                                         |
| -------------- | ---------------------------------------------------------------------------------------------------- |
| **Boost (dB)** | Adds `0–150 dB` of raw virtual-microphone gain before the final hard clamp; extreme values distort |

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

> **Note:** `Cycle Shared Hotkey Mode` is a global hotkey too, but its row
> lives with the setting it changes under `Hotkey Behaviour`, and appears only
> once `Multiple Sounds Per Hotkey` is on.

---

## Hotkey Behaviour

Open via `Settings` → `Control Hotkeys` → `Hotkey Behaviour`. Both switches are
off by default and work independently: either can be used without the other.

### Tab Hotkeys

- **Trigger:** Turn on `Tab Hotkeys`
- **What it does:** Each tab can be given its own hotkey, and while a tab is
  open only the sound hotkeys belonging to it respond

**Binding a tab:** right-click the tab in the sidebar → `Set Tab Hotkey`. A
tab's hotkey works from any tab, so there is always a way back. `General` can
be bound the same way.

**Binding a sound to one tab:** with the switch on, the sound hotkey dialog
gains an `Only while this tab is open` checkbox. Leave it unchecked and the
hotkey answers everywhere, which is what every hotkey assigned before this
feature does.

> **Notes:**
>
> - The same key combination can mean different sounds in different tabs, and
>   the same sound can take a different combination in each tab it appears in
> - The hotkey column shows the shortcut that applies in the tab you are
>   looking at; a tab's own shortcut wins over one that is live everywhere
> - `General` lists every sound, so a combination used in two tabs does nothing
>   while `General` is open rather than guessing between them
> - Folder tabs cannot be bound yet

### Multiple Sounds Per Hotkey

- **Trigger:** Turn on `Multiple Sounds Per Hotkey`
- **What it does:** Several sounds can share one hotkey

Assigning a combination another sound already answers to names that sound and
asks before adding to it. With the switch off the combination is refused, as
before.

### Shared Hotkey Mode

- **Trigger:** Select a mode, or use the `Cycle Shared Hotkey Mode` hotkey
- **What it does:** Decides which sound a shared hotkey plays

| Mode                    | Behavior                                          |
| ----------------------- | ------------------------------------------------- |
| `Play the same sound`   | Replays whichever member played last              |
| `Play the next sound`   | Advances through the members, one per press       |
| `Play a random sound`   | Picks a member at random                          |

> **Notes:**
>
> - This is separate from `Play Mode`, which decides what happens when a sound
>   *finishes* rather than what a press selects
> - The position `Play the next sound` keeps is per hotkey and resets when the
>   app restarts
> - A hotkey with one sound ignores the mode entirely

---

## System Tray

Linux Soundboard can sit in the system tray and keep running with its window
closed. This matters because global hotkeys live in the application itself:
with the tray on, closing the window leaves them working; with it off, closing
quits and the hotkeys stop with it.

Settings live under `Settings` → `General` → `System Tray`.

### Show Tray Icon

- **Default:** On
- **What it does:** Puts an icon in the desktop's status area

> **Notes:**
>
> - Left-click shows or hides the window; right-click opens the menu
> - Takes effect immediately — no restart
> - Needs a desktop that supports `StatusNotifierItem`. KDE Plasma, XFCE, LXQt,
>   Cinnamon, MATE, Budgie and waybar all do. **GNOME does not on its own** and
>   needs the
>   [AppIndicator and KStatusNotifierItem Support](https://extensions.gnome.org/extension/615/appindicator-support/)
>   extension
> - Where no tray exists, nothing appears and nothing breaks

### Close Button Minimises To Tray

- **Default:** On
- **What it does:** The window's close button hides the window instead of
  quitting, leaving global hotkeys live

> **Notes:**
>
> - Only ever acted on while an icon is really showing, so the window is never
>   hidden with no way back. Without a tray the close button quits as usual
> - Launching the app again brings the hidden window back
> - Quit from the tray menu, or from the desktop's media controls, to exit for
>   real

### Tray Menu

| Row                      | What it does                          |
| ------------------------ | ------------------------------------- |
| `Show`/`Hide Linux Soundboard` | Toggles the window                |
| `Play / Pause`           | Same as the transport button          |
| `Stop All`               | Stops every playing sound             |
| `Mute Real Mic`          | Toggles microphone passthrough        |
| `Quit`                   | Shuts the application down            |

The menu is deliberately short: everything else is reachable from the window
and from [Global Control Hotkeys](#global-control-hotkeys).

### Show In Media Controls

- **Default:** Off
- **What it does:** Publishes the playing sound to the desktop's media controls
  — the panel widget that shows a track name with transport buttons

> **Notes:**
>
> - Off by default for a reason: with it on, the app is a media player as far
>   as the desktop is concerned, so the media keys and the now-playing display
>   can go to it rather than to whatever music was running
> - The controls stay in the panel for as long as the switch is on, reporting
>   `Stopped` between sounds. They have to: a soundboard clip is over in a
>   second or two, and controls that appear and vanish with it are impossible
>   to press
> - Turning the switch off removes the player and hands the media keys back
>   immediately
> - Unlike the tray icon, this works on GNOME with no extension
> - The panel's `Raise` action brings the window back, whatever the tray is
>   doing

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
