# Bug reports

[Documentation](README.md) · [Troubleshooting](TROUBLESHOOTING.md) · [Open an issue](https://github.com/germanua/Linux-SoundBoard/issues)

Report reproducible failures, regressions, and incorrect documentation through
GitHub Issues. Include the version and build channel so the maintainer can match
your report to the right source revision.

## Generate a report

Run the report command explicitly:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh   | bash -s -- report
```

For an already authenticated release installer:

```bash
./install.sh report --output report.txt
```

Do not omit `report`: a piped installer with no arguments can start installation.

The default report location is `~/linux-soundboard-bug-report-<date>.txt`. It
contains system and audio-service information, application diagnostics, engine
logs, library checks, and questions for you to complete.

The report attempts to replace your home path and username, but device names and
other identifying details can remain. **Read and redact it before sharing.** The
report is a local file; attach it to the issue yourself. Do not attach your full
`config.json`, `library.sqlite3`, private audio, credentials, or unrelated logs.

An optional reproduction capture starts the app with debug logging. That can
start its engine and change microphone routing according to your settings.
Finish active calls or recordings before choosing it.

## Describe the problem

Use this structure in the issue:

```text
Version / build channel:
Distribution and version:
Desktop and session (Wayland, X11, or XWayland):
Installation method:

Steps to reproduce:
1.
2.
3.

Expected result:
Actual result:
Frequency:
Last version known to work, if known:

Relevant error output / reviewed report:
Screenshots, if useful:
```

Confirm the problem on a relevant released version or identify the exact
revision you tested. Do not switch profiles or replace a working installation
just to report a bug. Search existing issues and
[troubleshooting](TROUBLESHOOTING.md) for the same symptom.

## Manual diagnostics

For an ordinary per-user AppImage installation:

```bash
cat /etc/os-release
printf 'Session: %s\n' "$XDG_SESSION_TYPE"
systemctl --user status pipewire wireplumber --no-pager
wpctl status -n
"$HOME/.local/opt/linux-soundboard/linux-soundboard" --diagnose
journalctl --user -u linux-soundboard-engine.service -n 100 --no-pager
```

If using a source build, substitute its executable path. For another profile,
use that profile's executable, service, and directories.

If the library is involved and `sqlite3` is available, use a read-only check so a
misspelled or missing path cannot create an empty database:

```bash
sqlite3 -readonly "$HOME/.config/linux-soundboard/library.sqlite3" 'PRAGMA integrity_check;'
sqlite3 -readonly "$HOME/.config/linux-soundboard/library.sqlite3" 'PRAGMA user_version;'
```

`integrity_check` returning `ok` checks SQLite structural integrity. It does not
prove that the app's schema, permissions, paths, or library contents are correct.

## UI and renderer issues

If useful for reproducing the problem, compare a normal launch with:

```bash
GSK_RENDERER=cairo "$HOME/.local/opt/linux-soundboard/linux-soundboard"
```

For an X11 fallback where the desktop provides it:

```bash
LSB_FORCE_X11=1 "$HOME/.local/opt/linux-soundboard/linux-soundboard"
```

Quit the previous GUI instance first. Say which launch changed the result. These
commands launch the app; they are not passive diagnostic queries.

## Debug logs

For GUI runtime output:

```bash
RUST_LOG=debug "$HOME/.local/opt/linux-soundboard/linux-soundboard"
```

The installed engine is a separate process; its service log may also be needed.
For memory diagnostics, the app supports `LSB_MEMORY_REPORT=1` and
`LSB_MEMORY_REPORT_PATH`. Share only files that were actually created and review
them for personal information first.

Attach screenshots separately; a text report cannot contain the image itself.
