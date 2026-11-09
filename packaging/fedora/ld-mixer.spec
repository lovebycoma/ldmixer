# Fedora / RHEL-family package. Build with packaging/fedora/build-rpm.sh,
# which creates the source tarball (with vendored crates, so the build runs
# offline) and calls rpmbuild. Remove with: ld-mixer uninstall (or dnf remove ld-mixer)

# Rust release builds carry no debug info, so there is nothing to split out.
%global debug_package %{nil}
%{!?_udevrulesdir:%global _udevrulesdir /usr/lib/udev/rules.d}

Name:           ld-mixer
Version:        0.5.0
Release:        1%{?dist}
Summary:        Unofficial mixer and monitor control for Audient iD interfaces
License:        MIT
URL:            https://github.com/lovebycoma/ldmixer
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  make
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
BuildRequires:  pkgconfig(libusb-1.0)
BuildRequires:  systemd-rpm-macros
Requires:       hicolor-icon-theme
# pw-loopback and pw-dump, for the virtual inputs/outputs
Recommends:     pipewire-utils

%description
lD Mixer (Linux iD Mixer) is an unofficial counterpart of the Audient iD
Mixer: Main, Cue A and Cue B mixes, output routing, loopback, the monitor
section and meters. Audio keeps playing while it is connected.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --frozen --offline

%check
cargo test --frozen --offline --lib

%install
make install DESTDIR=%{buildroot} PREFIX=/usr UDEVDIR=%{_udevrulesdir}
# %%license below installs the license file itself.
rm -f %{buildroot}%{_datadir}/licenses/ld-mixer/LICENSE

%post
udevadm control --reload >/dev/null 2>&1 || :
udevadm trigger --subsystem-match=usb --attr-match=idVendor=2708 >/dev/null 2>&1 || :

%postun
udevadm control --reload >/dev/null 2>&1 || :

%files
%license LICENSE
%{_bindir}/ld-mixer
%{_datadir}/applications/com.lovebycoma.ldmixer.desktop
%{_datadir}/icons/hicolor/scalable/apps/com.lovebycoma.ldmixer.svg
%{_datadir}/metainfo/com.lovebycoma.ldmixer.metainfo.xml
%{_datadir}/ld-mixer/manifest
%{_udevrulesdir}/70-ld-mixer.rules
