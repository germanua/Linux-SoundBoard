# Release Checklist

This checklist covers every step required to cut a release of Linux Soundboard
from a clean clone. Work through each section in order. Do not skip sections
for minor releases — each step catches a different class of problem.

## Before You Start

- [ ] You are on a clean branch with no uncommitted changes.
- [ ] `git status` shows a clean working tree.
- [ ] You have a recent `git fetch` from the remote.

---

## 1. Version Bump

The version lives in exactly one place: `src/Cargo.toml`.

- [ ] Update `version = "X.Y.Z"` in `src/Cargo.toml`.
- [ ] Update `packaging/rpm/linux-soundboard.spec`: bump `Version:` and add a `%changelog` entry.
- [ ] Update `packaging/aur/PKGBUILD`: bump `pkgver=`.
- [ ] Update `packaging/aur/.SRCINFO`: bump `pkgver =`.
- [ ] Update `packaging/debian/changelog`: add a new entry at the top with `dch -v X.Y.Z-1`.
- [ ] Update `packaging/flatpak/com.linuxsoundboard.app.metainfo.xml`: add a new `<release version="X.Y.Z" date="YYYY-MM-DD">` entry.
- [ ] Run `packaging/validate-metadata.sh` — all checks must pass before proceeding.

---

## 2. Code Quality Gate

Run from the repository root:

```bash
cd src
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
```

All commands must exit 0.

---

## 3. Architecture Guard

Verify forbidden dependency directions are not present:

```bash
cd src
# audio must not import init, ui, or commands
grep -r "crate::init::"     app/audio/ --include="*.rs" | grep -v loudness_acceptance
grep -r "crate::ui::"       app/audio/ --include="*.rs" | grep -v loudness_acceptance
grep -r "crate::commands::" app/audio/ --include="*.rs" | grep -v loudness_acceptance

# loudness_acceptance must not be publicly exported
grep -r "^pub use.*loudness_acceptance\|^pub mod loudness_acceptance" app/audio/ --include="*.rs"
```

Each command must produce no output (empty = pass).

---

## 4. Packaging Smoke Check

```bash
bash packaging/smoke-check.sh
```

All checks must pass (0 failed). Skipped checks (build tools absent) are noted
but do not block the release — run the corresponding per-format checks on the
target build machine instead.

---

## 5. Build Each Package Format

These steps require the respective build tooling installed on the target
machine. Each must produce a clean artifact with no errors.

### AppImage (any x86_64 Linux)

```bash
bash packaging/linux/package-appimage.sh
```

Produces:
- `dist/linux-soundboard-X.Y.Z-x86_64.AppImage`
- `dist/linux-soundboard-x86_64.AppImage` (stable symlink)

Post-build checks:
- [ ] AppImage is executable and mounts cleanly.
- [ ] `./dist/linux-soundboard-x86_64.AppImage --help` exits 0.
- [ ] Preflight check runs on launch: `SKIP_PREFLIGHT_CHECK=0 ./dist/linux-soundboard-x86_64.AppImage`.
- [ ] App launches, virtual mic appears in `wpctl status`.
- [ ] Icon appears correctly in the app launcher.

### Debian / Ubuntu (requires `dpkg-dev`, `debhelper`)

```bash
bash packaging/debian/package-deb.sh
```

Produces:
- `dist/linux-soundboard_X.Y.Z-1_amd64.deb`

Post-build checks:
- [ ] `dpkg -I dist/*.deb` shows correct version and dependencies.
- [ ] `dpkg -c dist/*.deb` shows expected file list (binary, icons, desktop, metainfo, service, policy, helper).
- [ ] Install on a clean Debian/Ubuntu VM: `sudo dpkg -i dist/*.deb`.
- [ ] App launches from application menu.
- [ ] Service is enabled: `systemctl --user is-enabled linux-soundboard-engine.service`.
- [ ] Uninstall: `sudo dpkg -r linux-soundboard` — verify service is disabled.

### RPM / Fedora (requires `rpm-build`)

```bash
bash packaging/rpm/package-rpm.sh
```

Produces:
- `dist/linux-soundboard-X.Y.Z-1.x86_64.rpm`

Post-build checks:
- [ ] `rpm -qp --info dist/*.rpm` shows correct version.
- [ ] `rpm -qp --list dist/*.rpm` shows expected file list.
- [ ] Install on a clean Fedora VM: `sudo rpm -i dist/*.rpm`.
- [ ] App launches from application menu.
- [ ] Service is globally enabled: `systemctl --global is-enabled linux-soundboard-engine.service`.
- [ ] Uninstall: `sudo rpm -e linux-soundboard`.

### Flatpak (requires `flatpak-builder` and GNOME SDK)

```bash
flatpak install flathub org.gnome.Platform//47 org.gnome.Sdk//47
flatpak-builder --force-clean build-dir packaging/flatpak/com.linuxsoundboard.app.yml
```

Post-build checks:
- [ ] Build completes with no errors.
- [ ] Install locally and launch: `flatpak-builder --run build-dir packaging/flatpak/com.linuxsoundboard.app.yml linux-soundboard`.
- [ ] Desktop icon resolves correctly.
- [ ] App does not request permissions beyond those listed in the manifest.
- [ ] File chooser portal works for adding sounds outside the pre-granted directories.

### AUR (requires an Arch Linux environment with `makepkg`)

```bash
cd packaging/aur
makepkg -si
```

Post-build checks:
- [ ] Build completes cleanly.
- [ ] `check()` phase passes (cargo test).
- [ ] Installed binary is at `/usr/bin/linux-soundboard`.
- [ ] Desktop file, metainfo, icons, service, swhkd policy are all installed.
- [ ] Service is globally enabled after install.

---

## 6. Installer Smoke Test (user installer)

Test `packaging/linux/install-user.sh` from a binary:

```bash
# Install
./packaging/linux/install-user.sh install ./src/target/release/linux-soundboard

# Verify status
./packaging/linux/install-user.sh status

# Repair (should be a no-op if nothing changed)
./packaging/linux/install-user.sh repair ./src/target/release/linux-soundboard

# Remove (use --yes to skip confirmation prompt in CI)
./packaging/linux/install-user.sh remove --yes
```

Checks:
- [ ] `install` puts binary at `~/.local/opt/linux-soundboard/linux-soundboard`.
- [ ] `install` places desktop file, icons, service, and policy files.
- [ ] `status` reports all managed files as present.
- [ ] `repair` finds nothing to update after a fresh install.
- [ ] `remove` removes all managed files and disables the service.
- [ ] `remove` does not delete `~/.config/linux-soundboard/config.json` (user data preserved).

---

## 7. Service Validation

```bash
systemd-analyze verify packaging/linux/linux-soundboard-engine.service
```

Must exit 0 with no errors.

---

## 8. Git Tag and Release

- [ ] All checks above passed.
- [ ] Commit version bump and packaging changes: `git commit -m "release: bump to X.Y.Z"`.
- [ ] Tag: `git tag -s vX.Y.Z -m "version X.Y.Z"` (or unsigned: `git tag vX.Y.Z`).
- [ ] Push tag: `git push origin vX.Y.Z`.
- [ ] Create GitHub release with tag `vX.Y.Z`.
- [ ] Upload release assets:
  - `dist/linux-soundboard-X.Y.Z-x86_64.AppImage`
  - `dist/linux-soundboard_X.Y.Z-1_amd64.deb`
  - `dist/linux-soundboard-X.Y.Z-1.x86_64.rpm`
- [ ] Verify asset names match what `docs/INSTALL.md` documents.
- [ ] Add checksums (SHA256) for each asset to the release notes.

---

## 9. Post-Release

### AUR update (stable package)

- [ ] Update `packaging/aur/PKGBUILD`: set `pkgver`, update `sha256sums` for the new tarball.
- [ ] Update `packaging/aur/.SRCINFO` to match.
- [ ] Push to the AUR git remote.

### Flatpak / Flathub update

- [ ] Regenerate `packaging/flatpak/cargo-sources.json` with `flatpak-cargo-generator.py` against the new `src/Cargo.lock`.
- [ ] Update the `com.linuxsoundboard.app` repository on Flathub with the new manifest and cargo sources.

### Documentation check

- [ ] `docs/INSTALL.md` references match the released asset names.
- [ ] `docs/CHANGELOG.md` is updated with release notes.
- [ ] `README.md` badge/version references are current if present.

---

## Quick Re-check After Any Packaging Change

Run this before merging any change to `packaging/`:

```bash
bash packaging/smoke-check.sh
```
