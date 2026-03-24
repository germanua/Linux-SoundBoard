# Flathub Submission Guide for Linux Soundboard

This document provides instructions for submitting Linux Soundboard to Flathub.

## Prerequisites

- Flatpak package built and tested locally
- GitHub account
- Flathub account (linked to GitHub)

## Submission Steps

### 1. Fork the Flathub Repository

```bash
# Fork https://github.com/flathub/flathub on GitHub
git clone https://github.com/YOUR_USERNAME/flathub.git
cd flathub
```

### 2. Create Application Directory

```bash
mkdir com.linuxsoundboard.app
cd com.linuxsoundboard.app
```

### 3. Copy Required Files

Copy these files from `packaging/flatpak/`:
- `com.linuxsoundboard.app.yml` (manifest)
- `com.linuxsoundboard.app.metainfo.xml` (AppStream metadata)
- `cargo-sources.json` (Cargo dependencies)

### 4. Add Flathub-Specific Files

Create `flathub.json`:
```json
{
  "only-arches": ["x86_64"]
}
```

### 5. Validate Metadata

```bash
# Install appstream-util
sudo apt install appstream-util  # Ubuntu/Debian
sudo dnf install libappstream-glib  # Fedora

# Validate
appstream-util validate-relax com.linuxsoundboard.app.metainfo.xml
```

### 6. Test Build on Flathub Infrastructure

```bash
# Build using Flathub's buildbot
flatpak-builder --install-deps-from=flathub --force-clean build-dir com.linuxsoundboard.app.yml
```

### 7. Create Pull Request

```bash
git add com.linuxsoundboard.app/
git commit -m "Add com.linuxsoundboard.app"
git push origin main
```

Then create a PR on GitHub to `flathub/flathub`.

## Review Process

1. Automated checks will run (linting, building)
2. Flathub reviewers will check:
   - AppStream metadata quality
   - Manifest correctness
   - License compliance
   - Security (finish-args permissions)
3. Address any feedback
4. Once approved, app will be published to Flathub

## Post-Submission

After approval:
- App will be available via: `flatpak install flathub com.linuxsoundboard.app`
- Updates: Push to the `com.linuxsoundboard.app` repository
- Flathub will auto-build on new commits

## Important Notes

- **License**: PolyForm Noncommercial is accepted by Flathub
- **Screenshots**: Add actual screenshots to the repository (not just URLs)
- **Updates**: Keep manifest in sync with upstream releases
- **Support**: Monitor Flathub issues for user reports

## Resources

- Flathub Documentation: https://docs.flathub.org/
- Flatpak Builder Docs: https://docs.flatpak.org/en/latest/flatpak-builder.html
- AppStream Guidelines: https://www.freedesktop.org/software/appstream/docs/
