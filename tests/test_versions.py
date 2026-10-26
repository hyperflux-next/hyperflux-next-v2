# SPDX-License-Identifier: GPL-2.0-only
"""Every place that writes down the product version agrees with the root
Cargo.toml, and scripts/set-version keeps it that way."""

import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
# (file, pattern with the version as group 1, whether this file must exist)
LOCATIONS = [
    ("integrations/openrgb/CMakeLists.txt", r"project\(hyperflux-next-openrgb\s+VERSION ([0-9.]+)", False),
    ("sdk/cpp/CMakeLists.txt", r"project\(hyperflux-sdk\s+VERSION ([0-9.]+)", False),
    ("sdk/python/pyproject.toml", r'(?m)^version = "([^"]+)"', False),
    ("packaging/arch/PKGBUILD", r"(?m)^pkgver=(\S+)$", True),
    ("packaging/debian/changelog", r"\Ahyperflux-next-v2 \(([^-]+)-\d+\) ", True),
    ("packaging/rpm/hyperflux-next-v2.spec", r"(?m)^Version:\s+(\S+)$", True),
    ("CHANGELOG.md", r"(?m)^## (\S+)$", True),
]


def product_version(root=ROOT):
    return tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]


def found_versions(root):
    for name, pattern, required in LOCATIONS:
        path = root / name
        if not path.exists():
            assert not required, f"{name} is missing"
            continue
        match = re.search(pattern, path.read_text())
        assert match, f"no version found in {name}"
        yield name, match.group(1)


@pytest.mark.parametrize("name,pattern,required", LOCATIONS, ids=[n for n, _, _ in LOCATIONS])
def test_version_matches(name, pattern, required):
    if not (ROOT / name).exists() and not required:
        pytest.skip(f"{name} is missing")
    assert dict(found_versions(ROOT))[name] == product_version()


def test_no_epoch():
    # The hyperflux-next-v2-* names have no older versions to sort above.
    assert not re.search(r"(?m)^epoch=", (ROOT / "packaging/arch/PKGBUILD").read_text())
    assert not re.search(r"(?m)^Epoch:", (ROOT / "packaging/rpm/hyperflux-next-v2.spec").read_text())
    assert re.match(r"\S+ \(\d", (ROOT / "packaging/debian/changelog").read_text())


def test_lock_has_workspace_version():
    lock = (ROOT / "Cargo.lock").read_text()
    for crate in ("hfx-proto", "hfx-bridge", "hyperfluxctl"):
        assert f'name = "{crate}"\nversion = "{product_version()}"' in lock


def test_set_version(tmp_path):
    names = ["Cargo.toml", "Cargo.lock"] + [n for n, _, _ in LOCATIONS if (ROOT / n).exists()]
    for name in names:
        (tmp_path / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / name, tmp_path / name)
    subprocess.run([sys.executable, ROOT / "scripts/set-version", "--root", tmp_path, "9.8.7"],
                   check=True, capture_output=True)
    assert product_version(tmp_path) == "9.8.7"
    assert {v for _, v in found_versions(tmp_path)} == {"9.8.7"}
    spec = (tmp_path / "packaging/rpm/hyperflux-next-v2.spec").read_text()
    assert " - 9.8.7-1\n- HyperFlux Next V2 9.8.7\n" in spec
    debian = (tmp_path / "packaging/debian/changelog").read_text()
    assert debian.startswith("hyperflux-next-v2 (9.8.7-1) unstable;")
    assert "pkgrel=1\n" in (tmp_path / "packaging/arch/PKGBUILD").read_text()
    # The old entries stay below the new ones.
    assert f"## {product_version()}" in (tmp_path / "CHANGELOG.md").read_text()
