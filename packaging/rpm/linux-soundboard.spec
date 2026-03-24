Name:           linux-soundboard
Version:        1.1.0
Release:        1%{?dist}
Summary:        Native Linux soundboard with virtual microphone support

License:        PolyForm-Noncommercial-1.0.0
URL:            https://github.com/germanua/Linux-SoundBoard
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.85
BuildRequires:  rust >= 1.85
BuildRequires:  gtk4-devel
BuildRequires:  libadwaita-devel
BuildRequires:  pulseaudio-libs-devel
BuildRequires:  libX11-devel
BuildRequires:  libXi-devel
BuildRequires:  pkgconfig
BuildRequires:  ImageMagick

Requires:       gtk4
Requires:       libadwaita
Requires:       pulseaudio-libs
Requires:       libX11
Requires:       libXi
Requires:       pipewire
Requires:       pipewire-pulseaudio
Requires:       wireplumber
Requires:       pulseaudio-utils

Recommends:     xorg-x11-server-Xwayland
Recommends:     xdg-desktop-portal-gtk

%description
A high-performance, native Linux soundboard built with Rust, GTK4, and
Libadwaita. Features include virtual microphone for routing audio to
Discord, OBS, Zoom, etc., mic passthrough, LUFS normalization, global
hotkeys, and modern GTK4/Libadwaita UI with PipeWire/PulseAudio integration.

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
done

# Install metainfo
install -Dm644 packaging/flatpak/com.linuxsoundboard.app.metainfo.xml \
    %{buildroot}%{_datadir}/metainfo/com.linuxsoundboard.app.metainfo.xml

%files
%license LICENSE
%{_bindir}/linux-soundboard
%{_datadir}/applications/com.linuxsoundboard.app.desktop
%{_datadir}/icons/hicolor/*/apps/com.linuxsoundboard.app.png
%{_datadir}/metainfo/com.linuxsoundboard.app.metainfo.xml

%changelog
* Mon Mar 24 2026 germanua <tony.avramnco@icloud.com> - 1.1.0-1
- New upstream release
- Add native Wayland support
- Improve AppImage compatibility
- Add distribution-specific packages
- Fix virtual microphone creation on modern distributions

* Sat Mar 22 2026 germanua <tony.avramnco@icloud.com> - 1.0.0-1
- Initial RPM release
