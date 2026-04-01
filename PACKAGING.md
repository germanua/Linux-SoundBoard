# Packaging and Release Guide

This guide is for maintainers building release artifacts, validating them, and publishing GitHub releases.

## Repository Layout

| Path | Purpose |
| --- | --- |
| `packaging/linux/package-appimage.sh` | Build AppImage bundles |
| `packaging/debian/package-deb.sh` | Build Debian packages |
| `packaging/rpm/package-rpm.sh` | Build RPM packages |
| `packaging/flatpak/package-flatpak.sh` | Build Flatpak bundles |
| `packaging/linux/package-release.sh` | Create a local release bundle |
| `scripts/install.sh` | End-user bootstrap installer |

## Source of Truth

Before you build, verify version metadata in the files that drive packaging:

- `src/Cargo.toml`
- `packaging/debian/changelog`
- `packaging/rpm/linux-soundboard.spec`
- `packaging/flatpak/com.linuxsoundboard.app.metainfo.xml`

If artifact filenames are already fixed for a release, keep the release upload aligned to the actual built files even if local metadata has advanced to a newer package revision.

## Build Requirements

Common tools:

- `git`
- `cargo` and `rustc` 1.85+
- `pkg-config`
- `ImageMagick`

Package-specific tools:

| Format | Required tools |
| --- | --- |
| AppImage | `curl`, `bash`, network access for linuxdeploy downloads |
| DEB | `dpkg-buildpackage`, `debhelper` |
| RPM | `rpmbuild` |
| Flatpak | `flatpak-builder`, `python3` |

## Build Commands

### AppImage

```bash
./packaging/linux/package-appimage.sh
```

Expected output in `dist/`:

- `linux-soundboard-x86_64.AppImage`
- versioned AppImage output when produced by the script

### Debian Package

```bash
./packaging/debian/package-deb.sh
```

Expected output in `dist/`:

- `linux-soundboard_<version>_<arch>.deb`

### RPM Package

```bash
./packaging/rpm/package-rpm.sh
```

Expected output in `dist/`:

- `linux-soundboard-<version>.<arch>.rpm`

### Flatpak Bundle

```bash
./packaging/flatpak/package-flatpak.sh
```

Expected output:

- `dist/linux-soundboard-<version>.flatpak`
- `flatpak-repo/`

## Local Validation

Validate each package on its target distro before uploading it:

### Debian and Ubuntu

```bash
sudo apt install ./linux-soundboard_1.1.0-1_amd64.deb
linux-soundboard
```

### Fedora

```bash
sudo dnf install ./linux-soundboard-1.1.0-1.x86_64.rpm
linux-soundboard
```

### AppImage

```bash
chmod +x ./dist/linux-soundboard-x86_64.AppImage
./dist/linux-soundboard-x86_64.AppImage
```

Test on both:

- a Wayland session with `swhkd`
- an X11 or XWayland session using the native X11 backend

If you validate inside VMware and see GTK UI freezes or a large memory jump, retest with `GSK_RENDERER=cairo`. That isolates renderer issues from package issues.

## Release Workflow

### 1. Generate Checksums

From the directory that holds the final artifacts:

```bash
sha256sum linux-soundboard-1.1.0-1.x86_64.rpm linux-soundboard_1.1.0-1_amd64.deb > SHA256SUMS.txt
sha256sum -c SHA256SUMS.txt
```

### 2. Create and Push the Tag

```bash
git tag -a v1.1.0 -m "Linux Soundboard v1.1.0"
git push origin v1.1.0
```

### 3. Create the Draft Release

```bash
gh release create v1.1.0 \
  --repo germanua/Linux-SoundBoard \
  --title "Linux Soundboard v1.1.0" \
  --draft \
  --generate-notes \
  /path/to/linux-soundboard-1.1.0-1.x86_64.rpm \
  /path/to/linux-soundboard_1.1.0-1_amd64.deb \
  /path/to/SHA256SUMS.txt
```

### 4. Update Assets if You Rebuild

```bash
gh release upload v1.1.0 \
  /path/to/linux-soundboard-1.1.0-1.x86_64.rpm \
  /path/to/linux-soundboard_1.1.0-1_amd64.deb \
  /path/to/SHA256SUMS.txt \
  --clobber
```

### 5. Publish the Release

```bash
gh release edit v1.1.0 --draft=false
```

## Host Requirements That Matter at Runtime

Package builds install the application. They do not replace host audio/session requirements.

Required host runtime stack:

- `pipewire`
- `pipewire-pulse` or `pipewire-pulseaudio`
- `wireplumber`
- `pulseaudio-utils`

For Wayland hotkeys:

- `swhkd` must be installed on the host
- Arch family: use `swhkd-bin` or `swhkd-git`
- Debian, Ubuntu, Fedora, openSUSE: use upstream installation guidance

Upstream `swhkd` installation notes:

- https://github.com/waycrate/swhkd/blob/main/INSTALL.md

## Documentation Checklist

When cutting a release, update:

- `README.md`
- `docs/INSTALL.md`
- `docs/CHANGELOG.md`
- `docs/TROUBLESHOOTING.md`
- screenshots in `assets/screenshots/` if the UI changed materially

## Troubleshooting Release Work

- If a draft release shows a temporary `untagged-...` URL, verify the real tag exists on `origin` and re-check the release after GitHub finishes reconciling the draft.
- If the `.deb` or `.rpm` filename does not match the docs, fix the docs or rebuild the package. Do not publish misleading install commands.
- If Fedora or Ubuntu VM tests fail but real hardware does not, treat renderer virtualization issues separately from package correctness.
