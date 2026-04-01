# Contributing

Linux Soundboard accepts code, packaging, testing, and documentation contributions.

For the full contributor workflow, use the maintained guide in [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).

## Quick Path

1. Fork the repository on GitHub.
2. Clone your fork and create a focused branch.
3. Install the development dependencies listed in [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).
4. Build and test locally:

```bash
cd src
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

5. If you changed user-visible behavior, also update the relevant docs:

- [README.md](README.md)
- [docs/INSTALL.md](docs/INSTALL.md)
- [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md)
- [PACKAGING.md](PACKAGING.md)

## Bug Reports

Include:

- distro and version
- session type (`Wayland` or `X11`)
- install method (`AUR`, `.deb`, `.rpm`, `AppImage`, source build)
- steps to reproduce
- logs or error output

Useful diagnostics:

```bash
cat /etc/os-release
echo "XDG_SESSION_TYPE=$XDG_SESSION_TYPE"
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
echo "DISPLAY=$DISPLAY"
systemctl --user status pipewire pipewire-pulse wireplumber
```

## Packaging and Release Work

If you touch packaging or release behavior, keep the docs and artifact names aligned with what is actually published. The maintainer workflow lives in [PACKAGING.md](PACKAGING.md).
