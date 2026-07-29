# Bug Reports

Linux Soundboard uses GitHub Issues for bug reports and regressions.

- Issues: https://github.com/germanua/Linux-SoundBoard/issues

## Generate a report

The installer collects everything below for you:

```bash
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/install.sh | bash
```

Choose **Make a bug report**, or run it directly:

```bash
./install.sh report
```

It writes `~/linux-soundboard-bug-report-<date>.txt` containing:

- **System report** — distro, kernel, session type, audio devices, audio services, swhkd
- **App report** — `--diagnose`, installed version, engine service state and log, library integrity, and what changed in your audio setup since install
- **Bug report blank** — questions for you to answer in your own words

Your home path and username are replaced before the file is written; sound-device
names are kept, because routing bugs cannot be diagnosed without them. Read the
file before sharing it.

If the problem is something you can reproduce on demand, the report offers to run
the app with debug logging while you do it, and appends the log. That starts the
app and its audio engine, so it changes your default microphone while it runs.

**Screenshots cannot go in the text file.** Take them, then drag the image files
into the GitHub issue description alongside the pasted report.

## Before Opening an Issue

1. Confirm the problem still happens on the latest published release or current branch build.
2. Check [TROUBLESHOOTING.md](TROUBLESHOOTING.md) for known install, renderer, audio, and hotkey problems.
3. Reproduce the issue with the smallest reliable set of steps.

## Include This Information

The generated report already contains all of it. Collect it by hand only if the
installer cannot run on your system.

- Distribution and version
- Session type: `Wayland`, `X11`, or `XWayland`
- Install method: `AUR`, `.deb`, `.rpm`, `AppImage`, or source build
- Exact steps to reproduce
- Expected behavior
- Actual behavior
- Relevant logs or terminal output

Useful system diagnostics:

```bash
cat /etc/os-release
echo "XDG_SESSION_TYPE=$XDG_SESSION_TYPE"
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
echo "DISPLAY=$DISPLAY"
systemctl --user status pipewire wireplumber
wpctl status -n
linux-soundboard --diagnose
```

If the issue is packaging-related, include the package filename you installed and the exact command used to install it.

If sounds, folders, tabs, or hotkey bindings are missing or wrong, include the
state of the library database:

```bash
ls -l ~/.config/linux-soundboard/
sqlite3 ~/.config/linux-soundboard/library.sqlite3 'PRAGMA integrity_check;'
sqlite3 ~/.config/linux-soundboard/library.sqlite3 'PRAGMA user_version;'
```

Do not attach `library.sqlite3` or `config.json` themselves. They contain the
full paths of your audio files, and the output above is enough to start.

If the issue is UI-related inside a VM, mention whether it reproduces with:

```bash
GSK_RENDERER=cairo linux-soundboard
```

and:

```bash
LSB_FORCE_X11=1 linux-soundboard
```

## Log Collection

For runtime diagnostics:

```bash
RUST_LOG=debug linux-soundboard
```

For memory diagnostics:

```bash
RUST_LOG=info LSB_MEMORY_REPORT=1 LSB_MEMORY_REPORT_PATH=/tmp/lsb_memory_report.json linux-soundboard
```

Then attach:

- `/tmp/lsb_memory_report.txt`
- `/tmp/lsb_memory_report.json`

if those files were generated.

## Scope

GitHub Issues are the supported feedback channel for this project. Use them for:

- reproducible bugs
- packaging regressions
- distro-specific runtime failures
- crash reports
- incorrect or outdated documentation
