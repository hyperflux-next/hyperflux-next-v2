# SPDX-License-Identifier: GPL-2.0-only
#
# One spec for Fedora and openSUSE Tumbleweed. Neither ships an OpenRGB
# that loads plugin API 5. On Fedora the plugin package is built for
# OpenRGB 1.1 from openrgb.org, whose Fedora 44 package runs on Fedora's
# own Qt 6. openSUSE has no such package and gets no plugin.

%global openrazer_version 3.12.4
%global openrazer_commit 1ef8a91712906a89fd332a2ed62bbd105315d6d1
%global openrgb_commit 8e16de27858cf2be6af54a33ce003df74cb9fd3e
%global hfx_make %{__make} OPENRAZER_GROUP=plugdev OPENRGB_QT=6 DOCDIR=%{_docdir}/hyperflux-next
# cargo strips the binaries in its release profile.
%global debug_package %{nil}
# The packages these take over from all sort below this: the hyperflux-next-*
# packages of 2.x and of the 1:1.0.0 test build, and the pre-release
# hyperflux-v2-linux-dkms. The old names are provided at this version, so
# no package obsoletes itself.
%global old_names 1:1.0.1

Name:           hyperflux-next-v2
Version:        1.0.2
Release:        1%{?dist}
Summary:        Razer devices on a HyperFlux V2 receiver
License:        GPL-2.0-only
URL:            https://github.com/hyperflux-next/hyperflux-next-v2
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz
Source1:        https://github.com/openrazer/openrazer/archive/%{openrazer_commit}.tar.gz#/openrazer-%{openrazer_commit}.tar.gz
%if 0%{?fedora}
Source2:        https://github.com/CalcProgrammer1/OpenRGB/archive/%{openrgb_commit}.tar.gz#/openrgb-%{openrgb_commit}.tar.gz
%endif

BuildRequires:  cargo >= 1.95
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  patch
BuildRequires:  python3
BuildRequires:  rust >= 1.95
BuildRequires:  systemd-rpm-macros
%if 0%{?fedora}
BuildRequires:  cmake
BuildRequires:  gcc-c++
BuildRequires:  qt6-qtbase-devel
%endif

%if 0%{?suse_version}
%global udev_package udev
%else
%global udev_package systemd-udev
%endif

%description
HyperFlux Next V2 lets OpenRazer, OpenRGB and the desktop's battery
widget reach Razer keyboards and mice paired to a Razer HyperFlux V2
receiver.

%package linux
Summary:        HyperFlux Next V2 bridge, CLI and base services
License:        GPL-2.0-only AND (MIT OR Apache-2.0)
Requires:       systemd
Requires:       %{udev_package}
Requires(pre):  systemd
Requires(post): systemd
Requires(posttrans): systemd
Provides:       hyperflux-next-linux = %{old_names}
Obsoletes:      hyperflux-next-linux < %{old_names}
# The receiver kernel modules of 2.x and of the pre-release: Core reaches
# the receiver through hidraw instead.
Provides:       hyperflux-next-dkms = %{old_names}
Obsoletes:      hyperflux-next-dkms < %{old_names}
Obsoletes:      hyperflux-v2-linux-dkms < %{old_names}
Conflicts:      hyperflux-v2-linux-dkms

%description linux
The HyperFlux Next V2 bridge and device keeper services, the hyperfluxctl
command, and the system users, sockets and udev rules they need. The
bridge reaches the receiver through raw HID, so no kernel module is
needed. This is the Core edition; install hyperflux-next-v2-full to use
the devices in OpenRazer apps.

%package openrazer-driver-dkms
Summary:        OpenRazer driver sources with the HyperFlux Next V2 transport, for DKMS
License:        GPL-2.0-or-later AND GPL-2.0-only
BuildArch:      noarch
Requires:       dkms
Requires:       systemd
Requires:       %{udev_package}
Requires(post): dkms
Requires(post): systemd
Requires(preun): dkms
Provides:       openrazer-kernel-modules-dkms = %{openrazer_version}
Provides:       hyperflux-openrazer-driver-abi = %{openrazer_version}
Conflicts:      openrazer-kernel-modules-dkms
Provides:       hyperflux-next-openrazer-driver-dkms = %{old_names}
Obsoletes:      hyperflux-next-openrazer-driver-dkms < %{old_names}

%description openrazer-driver-dkms
OpenRazer %{openrazer_version}'s kernel drivers with a patch that lets them
drive the devices the HyperFlux Next V2 bridge offers. It takes the place
of openrazer-kernel-modules-dkms, so the stock openrazer-daemon installs on
top of it.

%package full
Summary:        HyperFlux Next V2 Core plus OpenRazer support for the paired devices
BuildArch:      noarch
Requires:       hyperflux-next-v2-linux = %{version}-%{release}
Requires:       hyperflux-next-v2-openrazer-driver-dkms = %{version}-%{release}
Provides:       hyperflux-next-full = %{old_names}
Obsoletes:      hyperflux-next-full < %{old_names}
Suggests:       openrazer-daemon
Suggests:       python3-openrazer
Suggests:       polychromatic
%if 0%{?fedora}
# Installed along with Full only when OpenRGB 1.1 is there for it.
Recommends:     hyperflux-next-v2-openrgb-plugin = %{version}-%{release}
%endif

%description full
The udev rules and module list that let the HyperFlux Next V2 bridge offer
the paired devices to OpenRazer apps such as Polychromatic.

%if 0%{?fedora}
%package openrgb-plugin
Summary:        HyperFlux Next V2 plugin for OpenRGB
License:        GPL-2.0-only AND MIT
Requires:       hyperflux-next-v2-linux = %{version}-%{release}
# Built against OpenRGB 1.1's headers. Fedora's own OpenRGB is older.
# This is the openrgb.org package.
Requires:       (openrgb >= 1.1 with openrgb < 1.2~)
Enhances:       openrgb

%description openrgb-plugin
Shows the Razer devices paired with a HyperFlux V2 receiver in OpenRGB, and
adds a page with their batteries to OpenRGB's Information tab. Needs
OpenRGB 1.1 from openrgb.org.
%endif

%prep
%autosetup -n %{name}-%{version}

%build
# Fedora's build flags export RUSTFLAGS, which override the release profile
# in Cargo.toml: opt-level 3, full debug info and no stripping. Without
# them the binaries come out as small and stripped as on the other
# distributions, which is what the debug_package line above relies on.
unset RUSTFLAGS
OPENRAZER_ARCHIVE=%{SOURCE1} %{hfx_make} openrazer-tree
%{hfx_make} rust crate-notices
%if 0%{?fedora}
OPENRGB_ARCHIVE=%{SOURCE2} %{hfx_make} openrgb-plugin
%endif

%install
for package in linux openrazer-dkms full %{?fedora:openrgb-plugin}; do
    %{hfx_make} DESTDIR=%{buildroot} install-$package
done
%if 0%{?fedora}
# Unlike the Rust binaries, CMake keeps the plugin's symbols, and without a
# debug package nothing else strips them.
%{__strip} --strip-unneeded %{buildroot}%{_prefix}/lib/openrgb/plugins/hyperflux-next-openrgb.so
%endif

%pre linux
# Replacing hyperflux-next-linux: its %%preun disables the bridge after this
# package is in place. An empty directory notes that it was enabled, for
# %%posttrans to enable it again.
if [ "$1" -eq 1 ] && systemctl --quiet is-enabled hyperflux-next-bridge.service 2>/dev/null; then
    mkdir -p %{_localstatedir}/lib/rpm-state/hyperflux-next-v2-bridge-enabled
fi

%post linux
systemd-sysusers hyperflux-next.conf || :
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || :
    udevadm control --reload || :
fi

%preun linux
%systemd_preun hyperflux-next-bridge.service hyperflux-next-keeper.service

%posttrans linux
if rmdir %{_localstatedir}/lib/rpm-state/hyperflux-next-v2-bridge-enabled 2>/dev/null; then
    systemctl --no-reload enable hyperflux-next-bridge.service || :
fi

%postun linux
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || :
    udevadm control --reload || :
fi

%post openrazer-driver-dkms
if [ -f /usr/lib/tmpfiles.d/dkms.conf ]; then
    systemd-tmpfiles --create dkms.conf || :
fi
systemd-sysusers openrazer.conf || :
dkms add -m openrazer-driver -v %{openrazer_version} --rpm_safe_upgrade || :
# Build for every kernel that has headers, but leave installing to
# dkms.service at the next boot, as the install guides say.
for build in /lib/modules/*/build; do
    [ -f "$build/Makefile" ] || continue
    kernel=${build%/build}
    dkms build -m openrazer-driver -v %{openrazer_version} -k "${kernel##*/}" || :
done
if [ -d /run/systemd/system ]; then
    udevadm control --reload || :
fi

%preun openrazer-driver-dkms
dkms remove -m openrazer-driver -v %{openrazer_version} --all --rpm_safe_upgrade || :

%post full
if [ -d /run/systemd/system ]; then
    udevadm control --reload || :
fi

%files linux
%license LICENSE target/LICENSE-rust-crates
%{_bindir}/hyperfluxctl
%dir %{_prefix}/lib/hyperflux-next
%{_prefix}/lib/hyperflux-next/hyperflux-next-bridge
%{_prefix}/lib/hyperflux-next/hyperflux-next-keeper
%{_unitdir}/hyperflux-next-bridge.service
%{_unitdir}/hyperflux-next-keeper.service
%{_unitdir}/hyperflux-next-client@.service
%{_unitdir}/hyperflux-next-client@.socket
%{_sysusersdir}/hyperflux-next.conf
%{_udevrulesdir}/99-hyperflux-next.rules
%{_modulesloaddir}/hyperflux-next.conf
%{_mandir}/man1/hyperfluxctl.1*
%{_datadir}/bash-completion/completions/hyperfluxctl
%{_datadir}/zsh/site-functions/_hyperfluxctl
%{_datadir}/fish/vendor_completions.d/hyperfluxctl.fish
%dir %{_docdir}/hyperflux-next
%{_docdir}/hyperflux-next/troubleshooting.md
# The unit's StateDirectory=, made and owned by systemd when the bridge
# first starts. Fedora's rpm finds the owner in the sysusers file, but
# openSUSE's would require it from another package, so the owner recorded
# there is root.
%if 0%{?fedora}
%ghost %dir %attr(0700,hyperflux-next,hyperflux) %{_localstatedir}/lib/hyperflux-next
%else
%ghost %dir %attr(0700,root,root) %{_localstatedir}/lib/hyperflux-next
%endif

%files openrazer-driver-dkms
%license LICENSE
/usr/src/openrazer-driver-%{openrazer_version}/
%{_sysusersdir}/openrazer.conf
%{_udevrulesdir}/99-razer.rules
%{_prefix}/lib/udev/razer_mount

%files full
%license LICENSE
%dir %{_datadir}/hyperflux-next
%dir %{_datadir}/hyperflux-next/components
%{_datadir}/hyperflux-next/components/full.json
%{_udevrulesdir}/69-hyperflux-next-openrazer-access.rules
%{_udevrulesdir}/70-hyperflux-next-openrazer-keys.rules
%{_modulesloaddir}/hyperflux-next-openrazer.conf

%if 0%{?fedora}
%files openrgb-plugin
%license LICENSE integrations/openrgb/LICENSE-nlohmann-json
%dir %{_prefix}/lib/openrgb
%dir %{_prefix}/lib/openrgb/plugins
%{_prefix}/lib/openrgb/plugins/hyperflux-next-openrgb.so
%endif

%changelog
* Fri Oct 30 2026 offalexjackson777-stack <offalexjackson777-stack@users.noreply.github.com> - 1.0.2-1
- HyperFlux Next V2 1.0.2

* Fri Oct 30 2026 offalexjackson777-stack <offalexjackson777-stack@users.noreply.github.com> - 1.0.1-1
- HyperFlux Next V2 1.0.1

* Mon Oct 26 2026 offalexjackson777-stack <offalexjackson777-stack@users.noreply.github.com> - 1.0.0-1
- HyperFlux Next V2 1.0.0
