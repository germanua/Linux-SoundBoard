Name:           linux-soundboard
Version:        1.1.2
Release:        1%{?dist}
Summary:        Native Linux soundboard with virtual microphone support

License:        PolyForm-Noncommercial-1.0.0
URL:            https://github.com/germanua/Linux-SoundBoard
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.85
BuildRequires:  rust >= 1.85
BuildRequires:  gtk4-devel
BuildRequires:  libadwaita-devel
BuildRequires:  libX11-devel
BuildRequires:  libXi-devel
BuildRequires:  pkgconfig
BuildRequires:  ImageMagick

Requires:       gtk4
Requires:       libadwaita
Requires:       libX11
Requires:       libXi
Requires:       pipewire
Requires:       wireplumber

Recommends:     xorg-x11-server-Xwayland

%description
A high-performance, native Linux soundboard built with Rust, GTK4, and
Libadwaita. Features include virtual microphone for routing audio to
Discord, OBS, Zoom, etc., mic passthrough, LUFS normalization, global
hotkeys via swhkd on Wayland and via the native X11 backend on X11/XWayland, and modern GTK4/Libadwaita
UI with native PipeWire virtual microphone support.

%prep
%setup -q

%build
bash packaging/linux/generate-icons.sh assets/icons/icon.png
cargo build --release --manifest-path src/Cargo.toml

%install
rm -rf %{buildroot}

# Install binary
install -Dm755 src/target/release/linux-soundboard \
    %{buildroot}%{_bindir}/linux-soundboard

# Install desktop file
install -Dm644 packaging/rpm/linux-soundboard.desktop \
    %{buildroot}%{_datadir}/applications/com.linuxsoundboard.app.desktop

# Install icons
for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512; do
    install -Dm644 src/resources/icons/$size/apps/com.linuxsoundboard.app.png \
        %{buildroot}%{_datadir}/icons/hicolor/$size/apps/com.linuxsoundboard.app.png
    install -Dm644 src/resources/icons/$size/apps/linux-soundboard.png \
        %{buildroot}%{_datadir}/icons/hicolor/$size/apps/linux-soundboard.png
done

# Install metainfo
install -Dm644 packaging/flatpak/com.linuxsoundboard.app.metainfo.xml \
    %{buildroot}%{_datadir}/metainfo/com.linuxsoundboard.app.metainfo.xml

%files
%license LICENSE
%{_bindir}/linux-soundboard
%{_datadir}/applications/com.linuxsoundboard.app.desktop
%{_datadir}/icons/hicolor/*/apps/com.linuxsoundboard.app.png
%{_datadir}/icons/hicolor/*/apps/linux-soundboard.png
%{_datadir}/metainfo/com.linuxsoundboard.app.metainfo.xml

%post
echo "Configuring LinuxSoundBoard..."

# Set setuid bit on swhkd if it exists
if [ -f /usr/bin/swhkd ]; then
    chmod u+s /usr/bin/swhkd
    echo "✓ Configured swhkd with setuid permissions"
else
    echo "Warning: swhkd not found. Native Wayland hotkeys need a host-installed swhkd."
    echo "Fedora does not currently ship swhkd in the official package set."
    echo "Install it from upstream: https://github.com/waycrate/swhkd/blob/main/INSTALL.md"
    echo "X11 and XWayland sessions can use the native X11 backend without swhkd."
fi

# Ensure swhks is executable
if [ -f /usr/bin/swhks ]; then
    chmod +x /usr/bin/swhks
fi

echo "✓ LinuxSoundBoard configuration complete"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t %{_datadir}/icons/hicolor >/dev/null 2>&1 || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database %{_datadir}/applications >/dev/null 2>&1 || true
fi

%postun
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t %{_datadir}/icons/hicolor >/dev/null 2>&1 || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database %{_datadir}/applications >/dev/null 2>&1 || true
fi

%changelog
* Wed Apr 01 2026 germanua <noreply@linuxsoundboard.invalid> - 1.1.2-1
- Fixed launcher icon lookup for native packages and AppImage builds
- Installed icon aliases required by desktop search integrations
- Refreshed icon and desktop caches in RPM lifecycle scripts

* Wed Apr 01 2026 germanua <noreply@linuxsoundboard.invalid> - 1.1.1-1
- Patch release for packaging and release metadata sync
- Added third-party license notices and README acknowledgments
- Refreshed release package examples and install metadata

* Tue Mar 25 2026 germanua <noreply@linuxsoundboard.invalid> - 1.1.0-2
- Migrated from Portal to swhkd for universal hotkey support
- Added support for Wayland, X11, and TTY hotkeys
- Improved hotkey reliability with hot reload via SIGHUP
- Removed Portal backend dependency
- Added automatic swhkd configuration in post-install

* Mon Mar 24 2026 germanua <noreply@linuxsoundboard.invalid> - 1.1.0-1
- New upstream release
- Add native Wayland support
- Improve AppImage compatibility
- Add distribution-specific packages
- Fix virtual microphone creation on modern distributions

* Sat Mar 22 2026 germanua <noreply@linuxsoundboard.invalid> - 1.0.0-1
- Initial RPM release
