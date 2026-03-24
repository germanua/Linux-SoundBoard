# Contributing to Linux Soundboard

Thank you for your interest in contributing to Linux Soundboard! This document provides guidelines and instructions for contributing to the project.

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [How Can I Contribute?](#how-can-i-contribute)
- [Development Setup](#development-setup)
- [Building the Project](#building-the-project)
- [Testing](#testing)
- [Submitting Changes](#submitting-changes)
- [Style Guidelines](#style-guidelines)
- [Project Structure](#project-structure)

---

## Code of Conduct

This project follows a simple code of conduct:

- **Be respectful** and considerate of others
- **Be collaborative** and help each other
- **Be patient** with newcomers
- **Focus on constructive feedback**
- **Keep discussions on-topic**

---

## How Can I Contribute?

### Reporting Bugs

Before creating a bug report:
1. **Check existing issues** to avoid duplicates
2. **Try the latest version** to see if it's already fixed
3. **Check the [Troubleshooting Guide](TROUBLESHOOTING.md)**

When reporting a bug, include:
- **Distribution and version** (e.g., Ubuntu 24.04, Fedora 40)
- **Installation method** (AppImage, DEB, RPM, Flatpak, AUR)
- **Steps to reproduce** the issue
- **Expected behavior** vs **actual behavior**
- **Error messages** or logs (use `RUST_LOG=debug`)
- **System information**:
  ```bash
  cat /etc/os-release
  echo "Wayland: $WAYLAND_DISPLAY"
  echo "X11: $DISPLAY"
  systemctl --user status pipewire
  ```

### Suggesting Features

Feature requests are welcome! Please:
1. **Check existing issues** for similar requests
2. **Explain the use case** - why is this feature needed?
3. **Describe the solution** you'd like
4. **Consider alternatives** you've thought about

### Improving Documentation

Documentation improvements are always appreciated:
- Fix typos or unclear instructions
- Add missing information
- Improve examples
- Translate documentation (future)

### Contributing Code

See [Development Setup](#development-setup) below.

---

## Development Setup

### Prerequisites

**Required:**
- Rust 1.85 or later
- GTK4 development libraries
- Libadwaita development libraries
- PulseAudio development libraries
- Git

**Recommended:**
- PipeWire + WirePlumber (for testing virtual mic)
- X11 development libraries (for hotkeys)

### Install Dependencies

**Ubuntu/Debian:**
```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libpulse-dev \
  libx11-dev libxi-dev pkg-config imagemagick \
  pipewire pipewire-pulse wireplumber pulseaudio-utils
```

**Fedora:**
```bash
sudo dnf install gtk4-devel libadwaita-devel pulseaudio-libs-devel \
  libX11-devel libXi-devel pkg-config ImageMagick alsa-lib-devel \
  pipewire pipewire-pulseaudio wireplumber pulseaudio-utils
```

**Arch Linux:**
```bash
sudo pacman -S gtk4 libadwaita libpulse libx11 libxi pkgconf imagemagick \
  pipewire pipewire-pulse wireplumber
```

### Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### Clone the Repository

```bash
git clone https://github.com/germanua/Linux-SoundBoard.git
cd Linux-SoundBoard
```

---

## Building the Project

### Development Build

```bash
cd src
cargo build
```

### Release Build

```bash
cd src
cargo build --release
```

The binary will be at `src/target/release/linux-soundboard`.

### Run from Source

```bash
cd src
cargo run
```

### Run with Debug Logging

```bash
cd src
RUST_LOG=debug cargo run
```

---

## Testing

### Run Unit Tests

```bash
cd src
cargo test
```

### Run Clippy (Linter)

```bash
cd src
cargo clippy --all-targets --all-features -- -D warnings
```

### Check Formatting

```bash
cd src
cargo fmt -- --check
```

### Format Code

```bash
cd src
cargo fmt
```

### Test Virtual Microphone

After building, test the virtual mic:

```bash
./target/release/linux-soundboard
```

Check if virtual mic was created:
```bash
pactl list sources | grep Linux_Soundboard
```

### Test on Different Distributions

Use the build scripts:

```bash
# Fedora
./scripts/build-fedora.sh

# Test AppImage
./packaging/linux/package-appimage.sh
```

---

## Submitting Changes

### Fork and Branch

1. **Fork** the repository on GitHub
2. **Clone** your fork:
   ```bash
   git clone https://github.com/YOUR_USERNAME/Linux-SoundBoard.git
   cd Linux-SoundBoard
   ```
3. **Create a branch**:
   ```bash
   git checkout -b feature/your-feature-name
   # or
   git checkout -b fix/issue-description
   ```

### Make Changes

1. **Write code** following the [Style Guidelines](#style-guidelines)
2. **Test your changes** thoroughly
3. **Run tests and linters**:
   ```bash
   cd src
   cargo test
   cargo clippy
   cargo fmt
   ```
4. **Update documentation** if needed

### Commit Changes

Use clear, descriptive commit messages:

```bash
git add .
git commit -m "Add feature: description of what you added"
```

**Good commit messages:**
- `Fix: Virtual mic not created on Fedora 40`
- `Add: Support for additional audio formats`
- `Improve: Error messages for missing dependencies`
- `Docs: Update installation instructions for Ubuntu`

**Bad commit messages:**
- `fix bug`
- `update`
- `changes`

### Push and Create Pull Request

```bash
git push origin feature/your-feature-name
```

Then:
1. Go to your fork on GitHub
2. Click "New Pull Request"
3. Select your branch
4. Fill in the PR template:
   - **Description**: What does this PR do?
   - **Related Issues**: Link to issues (e.g., "Fixes #123")
   - **Testing**: How did you test this?
   - **Screenshots**: If UI changes

### Pull Request Review

- Be patient - reviews may take a few days
- Respond to feedback constructively
- Make requested changes in new commits
- Don't force-push after review starts

---

## Style Guidelines

### Rust Code Style

Follow standard Rust conventions:

```rust
// Use rustfmt (automatic)
cargo fmt

// Follow Rust naming conventions
struct MyStruct { }      // PascalCase for types
fn my_function() { }     // snake_case for functions
const MY_CONST: i32 = 1; // SCREAMING_SNAKE_CASE for constants

// Use meaningful names
let user_count = 10;     // Good
let x = 10;              // Bad

// Add comments for complex logic
// Calculate LUFS normalization factor
let factor = calculate_lufs_factor(audio_data);

// Use Result for error handling
fn load_config() -> Result<Config, String> {
    // ...
}
```

### Code Organization

- Keep functions small and focused
- Use modules to organize related code
- Avoid deep nesting (max 3-4 levels)
- Extract complex logic into separate functions

### Error Handling

```rust
// Use Result for recoverable errors
fn parse_config(path: &Path) -> Result<Config, String> {
    // ...
}

// Use descriptive error messages
Err(format!("Failed to load config from {}: {}", path.display(), e))

// Log errors appropriately
log::error!("Failed to create virtual mic: {}", e);
log::warn!("PipeWire not detected, virtual mic unavailable");
log::info!("Virtual microphone created successfully");
log::debug!("Audio buffer size: {}", buffer_size);
```

### UI Code

```rust
// Use GTK4 best practices
let button = gtk::Button::builder()
    .label("Play Sound")
    .css_classes(vec!["suggested-action"])
    .build();

// Connect signals clearly
button.connect_clicked(clone!(@weak state => move |_| {
    handle_play_sound(&state);
}));
```

### Documentation

```rust
/// Creates a virtual microphone using PulseAudio/PipeWire.
///
/// # Returns
///
/// Returns `VirtualMicStatus` indicating success or failure.
///
/// # Examples
///
/// ```
/// let status = create_virtual_mic();
/// if status.active {
///     println!("Virtual mic created!");
/// }
/// ```
pub fn create_virtual_mic() -> VirtualMicStatus {
    // ...
}
```

---

## Project Structure

```
Linux-SoundBoard/
├── .github/
│   └── workflows/        # CI/CD workflows
├── docs/                 # Documentation
│   ├── TROUBLESHOOTING.md
│   ├── CHANGELOG.md
│   └── CONTRIBUTING.md
├── scripts/              # Build and utility scripts
│   ├── build-fedora.sh
│   └── test-fedora.sh
├── packaging/            # Package configurations
│   ├── linux/           # AppImage
│   ├── debian/          # DEB packages
│   ├── rpm/             # RPM packages
│   ├── flatpak/         # Flatpak
│   └── aur/             # AUR (Arch)
├── src/                  # Rust source code
│   ├── src/
│   │   ├── main.rs      # Entry point
│   │   ├── lib.rs       # Library root
│   │   ├── bootstrap.rs # App initialization
│   │   ├── app_state.rs # Global state
│   │   ├── ui/          # UI components
│   │   ├── audio/       # Audio engine
│   │   ├── pipewire/    # Virtual mic
│   │   ├── hotkeys/     # Global hotkeys
│   │   ├── config/      # Configuration
│   │   └── commands/    # Business logic
│   ├── ui/              # GTK UI files
│   ├── resources/       # Icons, assets
│   └── Cargo.toml       # Dependencies
├── assets/               # Project assets
│   ├── icons/
│   └── screenshots/
├── README.md
├── PACKAGING.md
└── LICENSE
```

### Key Modules

- **`ui/`**: GTK4 UI components (windows, dialogs, widgets)
- **`audio/`**: Audio playback, normalization, file handling
- **`pipewire/`**: Virtual microphone creation and management
- **`hotkeys/`**: Global hotkey system (X11 and Portal backends)
- **`config/`**: Configuration loading, saving, defaults
- **`commands/`**: Business logic (play sound, manage library, etc.)

---

## Development Tips

### Debugging

```bash
# Run with debug logging
RUST_LOG=debug cargo run

# Run with trace logging (very verbose)
RUST_LOG=trace cargo run

# Debug specific module
RUST_LOG=linux_soundboard::pipewire=debug cargo run

# Use rust-gdb for debugging
rust-gdb target/debug/linux-soundboard
```

### Testing Virtual Mic

```bash
# Check if virtual mic exists
pactl list sources | grep Linux_Soundboard

# Monitor virtual mic output
pactl subscribe

# Test audio routing
parecord --device=Linux_Soundboard_Mic test.wav
```

### Hot Reload (for UI changes)

Unfortunately, GTK4 doesn't support hot reload. You'll need to restart the app after changes.

### Performance Profiling

```bash
# Build with debug symbols
cargo build --release --profile release-with-debug

# Profile with perf
perf record -g ./target/release/linux-soundboard
perf report
```

---

## Getting Help

- **GitHub Discussions**: Ask questions, share ideas
- **GitHub Issues**: Report bugs, request features
- **Documentation**: Check docs/ folder

---

## License

By contributing, you agree that your contributions will be licensed under the same license as the project (PolyForm Noncommercial 1.0.0).

---

## Recognition

Contributors will be recognized in:
- GitHub contributors page
- Release notes (for significant contributions)
- README (for major features)

---

Thank you for contributing to Linux Soundboard! 🎵
