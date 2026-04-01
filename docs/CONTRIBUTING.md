# Contributing

Linux Soundboard accepts code, packaging, documentation, and testing contributions. This guide is the baseline workflow for contributors working on the repository.

## Before You Open an Issue

For bug reports:

1. Confirm the problem still happens on the latest code or latest published release.
2. Check [TROUBLESHOOTING.md](TROUBLESHOOTING.md) for known environment and packaging issues.
3. Collect the distro, desktop session type, installation method, and steps to reproduce.

Useful system info:

```bash
cat /etc/os-release
echo "XDG_SESSION_TYPE=$XDG_SESSION_TYPE"
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
echo "DISPLAY=$DISPLAY"
systemctl --user status pipewire pipewire-pulse wireplumber
```

For feature requests:

- describe the workflow that is missing
- explain why the current behavior is insufficient
- include any Linux desktop or audio-stack constraints that matter

## Development Environment

Required tooling:

- `git`
- Rust 1.85+
- GTK4 and Libadwaita development headers
- PulseAudio development libraries
- `pkg-config`
- `ImageMagick`

Recommended for local testing:

- PipeWire and WirePlumber
- `swhkd` for Wayland hotkey testing
- X11 development libraries for native X11 backend work

### Debian and Ubuntu

```bash
sudo apt install build-essential cargo rustc pkg-config imagemagick \
  libgtk-4-dev libadwaita-1-dev libpulse-dev libasound2-dev \
  libx11-dev libxi-dev pipewire pipewire-pulse wireplumber pulseaudio-utils
```

### Fedora

```bash
sudo dnf install cargo rust gcc gcc-c++ clang pkg-config ImageMagick \
  gtk4-devel libadwaita-devel pulseaudio-libs-devel alsa-lib-devel \
  libX11-devel libXi-devel pipewire pipewire-pulseaudio wireplumber pulseaudio-utils
```

### Arch Linux

```bash
sudo pacman -S cargo rust pkgconf imagemagick gtk4 libadwaita libpulse \
  alsa-lib libx11 libxi pipewire pipewire-pulse wireplumber
```

## Clone and Build

```bash
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard/src
cargo build
```

Release build:

```bash
cargo build --release
```

Run locally:

```bash
cargo run
```

Run with logs:

```bash
RUST_LOG=debug cargo run
```

## Tests and Checks

Run these before opening a pull request:

```bash
cd src
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

If you changed packaging or runtime behavior, also test an actual app launch.

## Repository Areas

| Path | Purpose |
| --- | --- |
| `src/` | Rust application source |
| `assets/` | Branding, screenshots, and icon source assets |
| `docs/` | End-user and contributor documentation |
| `packaging/` | Native packaging metadata and build scripts |
| `scripts/` | Helper scripts such as installer and distro test helpers |

## Preferred Contribution Workflow

1. Fork the repository in GitHub.
2. Clone the fork using the URL GitHub provides for your account.
3. Create a topic branch from the branch you intend to target.
4. Make the smallest coherent change that solves the problem.
5. Run format, lint, and test commands.
6. Update docs when behavior, install flow, packaging, or user-visible UI changes.
7. Open a pull request with:
   - the problem statement
   - the implementation summary
   - how you tested it
   - screenshots if the UI changed

## Packaging Contributions

If you change release or packaging behavior, update the matching files:

- `packaging/debian/`
- `packaging/rpm/`
- `packaging/flatpak/`
- `PACKAGING.md`
- `docs/INSTALL.md`

Do not claim a package format is published unless the corresponding release asset or public distribution channel actually exists.

## Documentation Contributions

Keep docs split by purpose:

- `README.md` for project overview and entry points
- `docs/INSTALL.md` for install and first-run guidance
- `docs/SCREENSHOTS.md` for the maintained screenshot gallery
- `docs/TROUBLESHOOTING.md` for operational fixes
- `PACKAGING.md` for maintainer packaging workflows
- `docs/CHANGELOG.md` for release history

If you update screenshots, use the files under `assets/screenshots/` and keep `assets/screenshots/README.md` aligned with the current set.

## Commit Messages

Write commit messages that explain the change, not just the file touched.

Good examples:

- `Document release packaging workflow`
- `Fix VMware renderer fallback for GTK startup`
- `Clarify Wayland hotkey requirements in install guide`

## Review Expectations

Pull requests are easier to review when they:

- stay focused on one problem
- explain test coverage or manual verification
- avoid mixing formatting churn with behavior changes
- call out distro-specific behavior explicitly when relevant
