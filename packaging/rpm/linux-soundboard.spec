%global debug_package %{nil}

Name:           linux-soundboard
Version:        2.2.0
Release:        1
Summary:        Native Linux soundboard with virtual microphone support

License:        PolyForm-Noncommercial-1.0.0
URL:            https://github.com/germanua/Linux-SoundBoard
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.85
BuildRequires:  rust >= 1.85
BuildRequires:  clang-devel
BuildRequires:  gtk4-devel
BuildRequires:  libadwaita-devel
BuildRequires:  pulseaudio-libs-devel
BuildRequires:  alsa-lib-devel
BuildRequires:  opus-devel
BuildRequires:  pipewire-devel
BuildRequires:  libX11-devel
BuildRequires:  libXi-devel
BuildRequires:  pkgconfig
BuildRequires:  ImageMagick
BuildRequires:  systemd-rpm-macros

Requires:       gtk4
Requires:       libadwaita
Requires:       pulseaudio-libs
Requires:       libX11
Requires:       libXi
Requires:       pipewire
Requires:       wireplumber
Requires:       polkit

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
export RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }--remap-path-prefix=$(pwd)=. --remap-path-prefix=${HOME}=~"
cargo build --release --manifest-path src/Cargo.toml

%install
rm -rf %{buildroot}

# Install binary
install -Dm755 target/release/linux-soundboard \
    %{buildroot}%{_bindir}/linux-soundboard

for legal_file in NOTICE.md THIRDPARTY_LICENSES.md THIRD_PARTY_NOTICES.html COMMERCIAL-LICENSE.md DONATIONS.md; do
    install -Dm644 $legal_file \
        %{buildroot}%{_docdir}/%{name}/$legal_file
done

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

# Install one-click Wayland hotkey installer helper and policy
install -Dm755 packaging/linux/install-swhkd-helper.sh \
    %{buildroot}%{_libexecdir}/linux-soundboard/install-swhkd-helper.sh
install -Dm644 packaging/linux/com.linuxsoundboard.install-swhkd.policy \
    %{buildroot}%{_datadir}/polkit-1/actions/com.linuxsoundboard.install-swhkd.policy

# Install user service for boot-ready audio engine
install -Dm644 packaging/linux/linux-soundboard-engine.service \
    %{buildroot}%{_userunitdir}/linux-soundboard-engine.service
install -Dm644 packaging/linux/linux-soundboard-engine.target \
    %{buildroot}%{_userunitdir}/linux-soundboard-engine.target

%files
%license LICENSE
%{_docdir}/%{name}/NOTICE.md
%{_docdir}/%{name}/THIRDPARTY_LICENSES.md
%{_docdir}/%{name}/THIRD_PARTY_NOTICES.html
%{_docdir}/%{name}/COMMERCIAL-LICENSE.md
%{_docdir}/%{name}/DONATIONS.md
%{_bindir}/linux-soundboard
%{_datadir}/applications/com.linuxsoundboard.app.desktop
%{_datadir}/icons/hicolor/*/apps/com.linuxsoundboard.app.png
%{_datadir}/icons/hicolor/*/apps/linux-soundboard.png
%{_datadir}/metainfo/com.linuxsoundboard.app.metainfo.xml
%{_libexecdir}/linux-soundboard/install-swhkd-helper.sh
%{_datadir}/polkit-1/actions/com.linuxsoundboard.install-swhkd.policy
%{_userunitdir}/linux-soundboard-engine.service
%{_userunitdir}/linux-soundboard-engine.target

%post
echo "Configuring LinuxSoundBoard..."
rm -f %{_datadir}/pipewire/pipewire.conf.d/99-linuxsoundboard.conf
if command -v systemctl >/dev/null 2>&1; then
    systemctl --global disable linux-soundboard-engine.service >/dev/null 2>&1 || true
    systemctl --global enable linux-soundboard-engine.target >/dev/null 2>&1 || true
fi

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
* Wed Jul 29 2026 germanua <114772595+germanua@users.noreply.github.com> - 2.2.0-1
- Store the sound library in SQLite and migrate existing configurations
- Show the complete folder hierarchy in the sidebar
- Remove, restore, reorder, and combine sidebar folders
- Cancel the folder scan started by Add Folder
- Cut startup and wide-library latency and memory

* Sun Jul 19 2026 germanua <114772595+germanua@users.noreply.github.com> - 2.1.2-1
- Add correct mono and stereo Ogg Opus support
- Default new configurations to Dynamic auto-gain
- Show live Analyze and Refine status counts
- Fix folder tabs, stream endings, and loudness analysis recovery

* Wed Jul 15 2026 germanua <114772595+germanua@users.noreply.github.com> - 2.1.1-1
- Restart stale installed engines and verify the running app version
- Restore a safe default microphone after temporary execution
- Protect schema-6 upgrades with an exact private backup
- Require explicit AppImage installation approval

* Sun Jul 12 2026 germanua <114772595+germanua@users.noreply.github.com> - 2.1.0-1
- Added folder-derived tabs and Delete-key removal
- Fixed mixed-version engine startup and Ogg Vorbis playback
- Hardened folder refresh, removal, and configuration persistence

* Wed Jun 10 2026 germanua <114772595+germanua@users.noreply.github.com> - 2.0.2-1
- Rebuilt swhkd from upstream source when packaged builds require pkexec
- Improved Wayland hotkey startup diagnostics and recovery guidance
- Fixed the hotkey capture dialog panic after backend startup failures

* Sat May 09 2026 germanua <114772595+germanua@users.noreply.github.com> - 2.0.0-1
- Promoted the testing branch rework to the main release line
- Added atomic replace playback to remove stop/play snapshot races
- Fixed continue play mode, close-time stop handling, and headphone icon states
- Updated swhkd hotkey formatting for current native Wayland hotkey handling

* Wed Apr 01 2026 germanua <114772595+germanua@users.noreply.github.com> - 1.1.2-1
- Fixed launcher icon lookup for native packages and AppImage builds
- Installed icon aliases required by desktop search integrations
- Refreshed icon and desktop caches in RPM lifecycle scripts

* Wed Apr 01 2026 germanua <114772595+germanua@users.noreply.github.com> - 1.1.1-1
- Patch release for packaging and release metadata sync
- Added third-party license notices and README acknowledgments
- Refreshed release package examples and install metadata

* Wed Mar 25 2026 germanua <114772595+germanua@users.noreply.github.com> - 1.1.0-2
- Migrated from Portal to swhkd for universal hotkey support
- Added support for Wayland, X11, and TTY hotkeys
- Improved hotkey reliability with hot reload via SIGHUP
- Removed Portal backend dependency
- Added automatic swhkd configuration in post-install

* Tue Mar 24 2026 germanua <114772595+germanua@users.noreply.github.com> - 1.1.0-1
- New upstream release
- Add native Wayland support
- Improve AppImage compatibility
- Add distribution-specific packages
- Fix virtual microphone creation on modern distributions

* Sun Mar 22 2026 germanua <114772595+germanua@users.noreply.github.com> - 1.0.0-1
- Initial RPM release
