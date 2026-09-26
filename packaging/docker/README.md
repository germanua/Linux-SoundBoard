# Containerized release builds

Current Linux Soundboard releases use the AppImage as the supported release artifact. The AppImage is built inside Ubuntu 24.04 and therefore has a glibc 2.39 ABI baseline.

```bash
packaging/docker/build-appimage.sh
```

The script stages an isolated copy of the working tree, builds inside `ubuntu:24.04`, and copies the versioned and stable AppImages into `dist/`. It requires Docker, `rsync`, and network access. Override the image with `APPIMAGE_BUILD_IMAGE` if necessary.

## Legacy helpers

`build-deb-appimage.sh` and `build-rpm.sh` are retained for historical maintenance only. They are **not part of the current release pipeline**. This does not pre-decide the distribution policy of a later release.

The AppImage can also be built directly with `packaging/linux/package-appimage.sh` on a host whose glibc baseline is suitable for distribution.
