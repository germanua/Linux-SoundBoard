# Getting started

[Documentation](README.md) · [Installation](INSTALL.md) · [Feature reference](FEATURE_REFERENCE.md)

This walkthrough uses the ordinary Linux Soundboard profile. Start with the
[installation guide](INSTALL.md) if the app is not installed.

## 1. Open the app

Launch **Linux Soundboard** from your application menu. For a per-user AppImage
installation, the terminal command is:

```bash
"$HOME/.local/opt/linux-soundboard/linux-soundboard"
```

A directly launched AppImage may offer **Install for persistent virtual mic**,
**Run temporarily**, or **Exit**. Temporary mode keeps the engine tied to that
launch; an installed user engine can keep the virtual microphone available
after the window closes. Closing to the tray is also different from quitting.

## 2. Add a few sounds

Drag supported audio files into the library, or add a folder through **Settings →
General → Sound Folders**. Double-click a sound, or press **Enter** on its selected
row, to play it. Check the [format reference](FEATURE_REFERENCE.md#supported-audio-formats)
if a file is not recognized.

Start with one short clip at a modest volume so you can check both output paths.
The app references your audio files; moving a file can require locating it again.

## 3. Select the virtual microphone

Choose **Linux_Soundboard_Mic** in your call, game, recording app, or input-device
picker. Local playback and the virtual-microphone feed have separate controls:

| Control | Effect |
| --- | --- |
| Headphones volume / output toggle | What you hear through local speakers or headphones. |
| Microphone volume | The soundboard feed sent to the virtual microphone. |
| Mic passthrough toggle | Whether your real microphone is mixed into that feed. |
| Settings → General → Microphone Source | Which microphone or supported virtual source supplies passthrough. |

**Default** microphone routing in the ordinary profile claims the system's
default input. Apps using the default input may therefore receive the soundboard.
Use **Manual** if you want to manage the system default yourself, then select the
virtual microphone explicitly in the intended app.

If you can hear the clip but the other app cannot, check its selected input and
follow [audio troubleshooting](TROUBLESHOOTING.md#audio-problems).

## 4. Add global hotkeys

Right-click a sound and choose **Set Hotkey**. Global transport shortcuts live in
**Settings → Control Hotkeys**. Avoid combinations already used by your desktop
or target application.

Wayland global hotkeys use the managed `swhkd` helper. If the app requests setup,
follow [Wayland installation](INSTALL.md#wayland-and-global-hotkeys). This helper
may require an administrator prompt. The X11 backend uses XInput2.

## 5. Control playback

The transport bar controls the active sound. **Stop All** stops all playback.
**Concurrent Playback** is optional and off by default; enable it under
**Settings → Audio → Playback** to mix more than one sound.

With at least two active playbacks, the compact **Now Playing** panel offers a
separate timeline, pause/resume, and stop control for each playback. Repeated
plays of the same file remain separate. The timelines use equal-size tracks;
a longer sound advances more slowly through its track.

See [transport controls](FEATURE_REFERENCE.md#transport-bar) for seeking,
play modes, and volume behavior.

## Keep your library recoverable

Settings live in `config.json`; sounds, tabs, folders, and hotkey bindings live
in `library.sqlite3` under your profile's configuration directory. Copying only
`config.json` does not back up the library. Audio files remain at their original
locations and need their own backup.

Before manually copying a profile, quit the GUI and stop its matching engine.
Keep the full directory, including any SQLite companion files. Use the
[recovery guidance](TROUBLESHOOTING.md#sound-library-could-not-be-opened) before
replacing files after a migration or database error.
