# Bug Reports

Linux Soundboard uses GitHub Issues for bug reports and regressions.

- Issues: https://github.com/germanua/Linux-SoundBoard/issues

## Before Opening an Issue

1. Confirm the problem still happens on the latest published release or current branch build.
2. Check [TROUBLESHOOTING.md](TROUBLESHOOTING.md) for known install, renderer, audio, and hotkey problems.
3. Reproduce the issue with the smallest reliable set of steps.

## Include This Information

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
```

If the issue is packaging-related, include the package filename you installed and the exact command used to install it.

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
RUST_LOG=info LSB_MEMORY_REPORT=1 LSB_MEMORY_REPORT_PATH=/tmp/lsb-report.json linux-soundboard
```

Then attach:

- `/tmp/lsb-report.txt`
- `/tmp/lsb-report.json`

if those files were generated.

## Scope

GitHub Issues are the supported feedback channel for this project. Use them for:

- reproducible bugs
- packaging regressions
- distro-specific runtime failures
- crash reports
- incorrect or outdated documentation
