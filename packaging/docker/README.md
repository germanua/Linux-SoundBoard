# Containerized package builds

Helpers for building the native packages on a host that is not the package's
target distribution (for example, building the `.deb`/`.rpm` on Arch).

Each script copies the working tree into an isolated build context, runs the
matching per-distro packaging script inside a container, and copies the finished
artifacts into `dist/` at the repository root. They require `docker` and `rsync`
on the host and network access inside the container.

| Script | Image | Produces |
|--------|-------|----------|
| `build-deb-appimage.sh` | `ubuntu:24.04` | `linux-soundboard_*_amd64.deb`, `linux-soundboard-*-x86_64.AppImage` |
| `build-rpm.sh` | `fedora:latest` | `linux-soundboard-*.x86_64.rpm` |

```bash
packaging/docker/build-rpm.sh
packaging/docker/build-deb-appimage.sh
```

Why containers are needed:

- The `.deb` requires the Debian toolchain (`dpkg-buildpackage`, `debhelper`).
- The `.rpm` spec uses Fedora-only macros (`%{_userunitdir}`) and an rpm database.
- The AppImage must be built against an older glibc than a rolling-release host
  provides; otherwise the bundled GTK libraries crash the dynamic loader on
  startup. Ubuntu 24.04 (glibc 2.39, GTK 4.14, libadwaita 1.5) keeps the AppImage
  portable while still satisfying the `gtk4`/`libadwaita` feature requirements.

The tarball and a locally runnable AppImage can also be built directly with
`packaging/linux/package-appimage.sh` when the host itself is a suitable base.
Override the base image with `DEB_BUILD_IMAGE` / `RPM_BUILD_IMAGE` if needed.
