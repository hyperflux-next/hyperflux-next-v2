# SPDX-License-Identifier: GPL-2.0-only
"""The packaging and the docs agree with the contracts they copy from:
error codes, devices, upstream pins, the bridge unit, the project URLs and
the package names they take over from."""

import re
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent


def read(name):
    return (ROOT / name).read_text()


def makefile_var(name):
    return re.search(rf"(?m)^{name} \?= (\S+)$", read("Makefile")).group(1)


def pin(name):
    path = ROOT / name
    if not path.exists():
        pytest.skip(f"{name} is missing")
    return dict(re.findall(r"(?m)^(\w+)=(\S+)$", path.read_text()))


def test_troubleshooting_sections():
    codes = re.findall(r'"(HFX-\d{3})"', read("crates/hfx-proto/src/errors.rs"))
    sections = re.findall(r"(?m)^### (HFX-\d{3})$", read("docs/user/troubleshooting.md"))
    assert codes and set(codes) == set(sections) and len(sections) == len(set(sections))


def test_hardware_page():
    page = read("docs/user/hardware.md")
    # The plugin rows only the OpenRGB 1.1 build has.
    table = read("integrations/openrgb/src/device_table.hpp")
    newer = re.findall(r"0x(\w{4})", "".join(re.findall(r"#if HFX_OPENRGB_QT >= 6\n(.*?)#endif", table, re.S)))
    for device in tomllib.loads(read("devices.toml"))["device"]:
        openrgb = "OpenRGB 1.1" if f"{device['product_id']:04x}" in newer else "OpenRGB"
        apps = [a for a, part in (("OpenRazer", "openrazer"), (openrgb, "openrgb")) if part in device]
        row = (f"| `1532:{device['product_id']:04x}` | `1532:{device['wired_product_id']:04x}` "
               f"| {device['tier']} | {', '.join(apps)} |")
        assert row.replace("|  |", "| |") in page.replace("|  |", "| |"), device["name"]


def test_openrazer_pin():
    openrazer = pin("integrations/openrazer/PIN")
    version, commit = openrazer["OPENRAZER_VERSION"], openrazer["OPENRAZER_COMMIT"]
    pkgbuild = read("packaging/arch/PKGBUILD")
    assert f"_openrazer_version={version}\n" in pkgbuild
    assert f"_openrazer_commit={commit}\n" in pkgbuild
    assert f"'{openrazer['OPENRAZER_SHA256']}'" in pkgbuild
    spec = read("packaging/rpm/hyperflux-next-v2.spec")
    assert f"%global openrazer_version {version}\n" in spec
    assert f"%global openrazer_commit {commit}\n" in spec
    assert f"openrazer-driver-dkms (= {version}+dfsg-1)" in read("packaging/debian/control")
    for script in ("postinst", "prerm"):
        assert version in read(f"packaging/debian/hyperflux-next-v2-openrazer-driver-dkms.{script}")
    assert f"openrazer={version}\n" in read("scripts/install-test.sh")


def test_openrgb_pin():
    openrgb = pin("integrations/openrgb/PIN")
    pkgbuild = read("packaging/arch/PKGBUILD")
    assert f"_openrgb_commit={openrgb['OPENRGB_QT6_COMMIT']}\n" in pkgbuild
    assert f"'{openrgb['OPENRGB_QT6_SHA256']}'" in pkgbuild
    spec = read("packaging/rpm/hyperflux-next-v2.spec")
    assert f"%global openrgb_commit {openrgb['OPENRGB_QT6_COMMIT']}\n" in spec


def test_openrgb_plugin_package():
    # Doctor looks for the plugin where the packages put it.
    plugin = re.search(r'OPENRGB_PLUGIN: &str = "(.+)";', read("crates/hfx-proto/src/paths.rs")).group(1)
    assert f"plugin={plugin}\n" in read("scripts/install-test.sh")
    spec = read("packaging/rpm/hyperflux-next-v2.spec")
    assert f"\n%{{_prefix}}{plugin.removeprefix('/usr')}\n" in spec
    # Full depends on it where the distribution's own OpenRGB loads it, and
    # only recommends it where it needs OpenRGB 1.1 from openrgb.org.
    pkgbuild = read("packaging/arch/PKGBUILD")
    full = pkgbuild.split("package_hyperflux-next-v2-full() {", 1)[1].split("\n}", 1)[0]
    assert '"hyperflux-next-v2-openrgb-plugin=$pkgver-$pkgrel"' in full
    rules = read("packaging/debian/rules")
    assert "hfx:Full-Depends=hyperflux-next-v2-openrgb-plugin (= $(DEB_VERSION))" in rules
    assert "hfx:Full-Recommends=hyperflux-next-v2-openrgb-plugin (= $(DEB_VERSION))" in rules
    assert "hfx:OpenRGB=openrgb (>= 1.1), openrgb (<< 1.2~)" in rules
    assert re.search(r"(?m)^Recommends:\s+hyperflux-next-v2-openrgb-plugin = %\{version\}-%\{release\}$", spec)
    assert re.search(r"(?m)^Requires:\s+\(openrgb >= 1\.1 with openrgb < 1\.2~\)$", spec)


def test_licence_in_every_package():
    # LICENSE is the GPL's text. Arch and RPM packages install it. Every .deb carries Debian's
    # copyright file instead, which points at the system's copy, as lintian wants.
    assert read("LICENSE").startswith("GNU GENERAL PUBLIC LICENSE\nVersion 2, June 1991\n")
    pkgbuild = read("packaging/arch/PKGBUILD")
    for name in re.search(r"(?ms)^pkgname=\((.*?)\)$", pkgbuild).group(1).split():
        body = pkgbuild.split(f"package_{name}() {{", 1)[1].split("\n}", 1)[0]
        assert '/usr/share/licenses/$pkgname" "$srcdir/$_srcdir/LICENSE"' in body, name
    sections = re.split(r"(?m)^%files ", read("packaging/rpm/hyperflux-next-v2.spec"))[1:]
    assert len(sections) == 4 and all(re.search(r"(?m)^%license LICENSE\b", s) for s in sections)
    copyright = r"(?s).*\nFiles: \*\n[^\n]*\nLicense: GPL-2.0-only\n.*/usr/share/common-licenses/GPL-2\."
    assert re.match(copyright, read("packaging/debian/copyright"))


# What the bridge and the keeper units share: notify with a watchdog,
# restarts, the stop after user sessions and the sandbox.
COMMON_UNIT = ("Type=notify", "NotifyAccess=main", "WatchdogSec=30s", "Group=hyperflux",
               "Restart=on-failure", "RestartSec=2s", "Before=systemd-user-sessions.service", "DevicePolicy=closed", "ProtectSystem=strict",
               "ProtectKernelTunables=yes", "RestrictAddressFamilies=AF_UNIX AF_NETLINK",
               "CapabilityBoundingSet=", "AmbientCapabilities=", "NoNewPrivileges=yes",
               "IPAddressDeny=any", "RemoveIPC=yes", "ProtectProc=invisible", "ProcSubset=pid",
               "UMask=0077", "SystemCallFilter=~@mount @privileged @resources",
               "Documentation=man:hyperfluxctl(1)")
UNBIND = [f"-/sys/bus/hid/drivers/{driver}/unbind"
          for driver in ("razeraccessory", "razerkbd", "razerkraken", "razermouse")]


def unit_has(unit, lines):
    for line in lines:
        assert re.search(rf"(?m)^{re.escape(line)}$", unit), line


def test_bridge_unit():
    unit = read("systemd/hyperflux-next-bridge.service")
    unit_has(unit, COMMON_UNIT + (
        "User=hyperflux-next", "SupplementaryGroups=hyperflux-next",
        "ExecStart=/usr/lib/hyperflux-next/hyperflux-next-bridge",
        "RuntimeDirectory=hyperflux-next", "RuntimeDirectoryMode=2750",
        "StateDirectory=hyperflux-next", "StateDirectoryMode=0700",
        "After=systemd-udevd.service", "DeviceAllow=char-hidraw rw",
        "Wants=hyperflux-next-keeper.service",
        "WantedBy=multi-user.target"))
    # The keeper's, not the bridge's.
    assert "DeviceAllow=/dev/uhid" not in unit and not any(path in unit for path in UNBIND)


def test_keeper_unit():
    unit = read("systemd/hyperflux-next-keeper.service")
    unit_has(unit, COMMON_UNIT + (
        "User=hyperflux-keeper", "SupplementaryGroups=hyperflux-keeper",
        "ExecStart=/usr/lib/hyperflux-next/hyperflux-next-keeper",
        "After=systemd-udevd.service hyperflux-next-bridge.service",
        "DeviceAllow=char-hidraw r", "DeviceAllow=/dev/uhid rw"))
    assert all(path in unit for path in UNBIND)
    # The bridge starts it, and the bridge's runtime directory is the bridge's.
    assert "[Install]" not in unit and "RuntimeDirectory" not in unit


def test_receiver_access():
    # Interface 0 is the bridge's to write and the keeper's to read,
    # interface 1 and /dev/uhid are the keeper's, and 4 is the bridge's.
    rules = read("udev/99-hyperflux-next.rules")
    for line in ('ATTRS{bInterfaceNumber}=="00", OWNER="hyperflux-next", GROUP="hyperflux-keeper", MODE="0640"',
                 'ATTRS{bInterfaceNumber}=="01", GROUP="hyperflux-keeper", MODE="0640"',
                 'ATTRS{bInterfaceNumber}=="04", GROUP="hyperflux-next", MODE="0660"'):
        assert f"\n{line}\n" in rules, line
    assert re.search(r'(?m)^KERNEL=="uhid".* GROUP="hyperflux-keeper", MODE="0660"', rules)
    access = read("integrations/openrazer/69-hyperflux-next-openrazer-access.rules")
    assert "chgrp hyperflux-keeper /sys%p/unbind" in access


def test_project_urls():
    project, site = makefile_var("PROJECT_URL"), makefile_var("SITE_URL")
    assert f"url='{project}'" in read("packaging/arch/PKGBUILD")
    assert re.search(rf"(?m)^URL:\s+{re.escape(project)}$", read("packaging/rpm/hyperflux-next-v2.spec"))
    assert f"Homepage: {project}\n" in read("packaging/debian/control")
    for page in (ROOT / "docs/user").glob("install-*.md"):
        assert site in page.read_text(), page.name
        assert "github.io" not in page.read_text().replace(site, ""), page.name


# What each package takes over from: the hyperflux-next-* names of 2.x and
# of the 1:1.0.0 test build, and for Core the old receiver kernel modules.
# The hyperflux-next-* names are also provided, so nothing that asks for
# them breaks.
TAKES_OVER = {
    "linux": {"hyperflux-next-linux", "hyperflux-next-dkms", "hyperflux-v2-linux-dkms"},
    "openrazer-driver-dkms": {"hyperflux-next-openrazer-driver-dkms"},
    "full": {"hyperflux-next-full"},
}


def provided(old):
    return {name for name in old if name.startswith("hyperflux-next-")}


def names(relations):
    """Package names without quotes and version constraints."""
    return {re.split(r"[\s=<>(]", r.strip().strip("'\""), maxsplit=1)[0] for r in relations if r.strip()}


def test_arch_old_names():
    pkgbuild = read("packaging/arch/PKGBUILD")
    for part, old in TAKES_OVER.items():
        body = pkgbuild.split(f"package_hyperflux-next-v2-{part}() {{", 1)[1].split("\n}", 1)[0]
        arrays = {key: names(re.findall(r"'[^']*'|\"[^\"]*\"", value))
                  for key, value in re.findall(r"(?ms)^  (\w+)=\((.*?)\)$", body)}
        assert old <= arrays["conflicts"] and old <= arrays["replaces"], part
        assert provided(old) <= arrays["provides"], part


def test_debian_old_names():
    stanzas = read("packaging/debian/control").split("\n\n")
    for part, old in TAKES_OVER.items():
        stanza = next(s for s in stanzas if s.startswith(f"Package: hyperflux-next-v2-{part}\n"))
        # Continuation lines start with a space.
        fields = dict(re.findall(r"(?m)^(\w+): (.*(?:\n .*)*)", stanza))
        relations = {key: names(fields.get(key, "").split(",")) for key in ("Conflicts", "Replaces", "Provides")}
        assert old <= relations["Conflicts"] and old <= relations["Replaces"], part
        assert provided(old) <= relations["Provides"], part


def test_rpm_old_names():
    spec = read("packaging/rpm/hyperflux-next-v2.spec")
    epoch, version = re.search(r"(?m)^%global old_names (\d+):(\S+)$", spec).groups()
    # Above every build of the old names: 2.x (no epoch) and 1:1.0.0-*.
    assert (int(epoch), [int(n) for n in version.split(".")]) > (1, [1, 0, 0])
    for part, old in TAKES_OVER.items():
        section = spec.split(f"\n%package {part}\n", 1)[1].split("\n%description", 1)[0]
        tags = re.findall(r"(?m)^(Provides|Obsoletes):\s+(\S+) (\S+) (\S+)$", section)
        obsoletes = {name: f"{op} {evr}" for tag, name, op, evr in tags if tag == "Obsoletes"}
        provides = {name: f"{op} {evr}" for tag, name, op, evr in tags if tag == "Provides"}
        # Obsoleted below the bound and provided at it: no package
        # obsoletes itself.
        assert all(obsoletes.get(name) == "< %{old_names}" for name in old), part
        assert all(provides.get(name) == "= %{old_names}" for name in provided(old)), part


def test_udev_attrs_match_one_device():
    # udev matches every ATTRS key of one rule against the same parent
    # device. The ids are attributes of the USB device and the interface
    # number of its interface, so a rule that asks for both matches nothing.
    device = {"idVendor", "idProduct", "serial", "manufacturer", "product"}
    interface = {"bInterfaceNumber", "bInterfaceClass", "bInterfaceSubClass", "bInterfaceProtocol"}
    files = [ROOT / "udev/99-hyperflux-next.rules", *sorted((ROOT / "integrations").glob("*/*.rules"))]
    for path in files:
        for rule in path.read_text().replace("\\\n", " ").splitlines():
            keys = set(re.findall(r"\bATTRS\{(\w+)\}", rule))
            assert not (keys & device and keys & interface), f"{path.name}: {rule}"
