# SPDX-License-Identifier: GPL-2.0-only
"""scripts/releases.py against fixed copies of the distributions' lists."""

import datetime
import importlib.util
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("releases", ROOT / "scripts" / "releases.py")
releases = importlib.util.module_from_spec(spec)
spec.loader.exec_module(releases)

BODHI = {"releases": [
    {"name": "ELN", "version": "eln", "id_prefix": "FEDORA", "branch": "eln", "state": "pending", "eol": None},
    {"name": "F43", "version": "43", "id_prefix": "FEDORA", "branch": "f43", "state": "current", "eol": "2026-12-02"},
    {"name": "F44", "version": "44", "id_prefix": "FEDORA", "branch": "f44", "state": "current", "eol": "2027-06-02"},
    {"name": "F45", "version": "45", "id_prefix": "FEDORA", "branch": "f45", "state": "current", "eol": "2027-12-08"},
    {"name": "F45C", "version": "45", "id_prefix": "FEDORA-CONTAINER", "branch": "f45", "state": "current", "eol": None},
    {"name": "F46", "version": "46", "id_prefix": "FEDORA", "branch": "rawhide", "state": "pending", "eol": None},
]}
DEBIAN = """version,codename,series,created,release,eol,eol-lts,eol-elts
13,Trixie,trixie,2023-06-10,2025-08-09,2028-08-09,2030-06-30,2035-06-30
14,Forky,forky,2025-08-09
15,Duke,duke,2027-08-01
,Sid,sid,1993-08-16
"""
UBUNTU = """version,codename,series,created,release,eol,eol-server,eol-esm,eol-legacy
26.04 LTS,Resolute Raccoon,resolute,2025-10-09,2026-04-23,2031-05-29,2031-05-29,2036-04-23,2041-04-30
26.10,Stonking Stingray,stonking,2026-04-24,2026-10-15,2027-07-15
27.04,Tenacious T-Rex,tenacious,2026-10-16,2027-04-22,2028-01-20
"""
FULL = "arch - -\nubuntu 26.10 stonking\ndebian 14 forky\nfedora 44 -\nfedora 45 -\nopensuse - -\n"


@pytest.fixture
def check(tmp_path, monkeypatch, capsys):
    pages = {releases.BODHI: json.dumps(BODHI), releases.DISTRO_INFO.format("debian"): DEBIAN,
             releases.DISTRO_INFO.format("ubuntu"): UBUNTU}
    monkeypatch.setattr(releases, "fetch", pages.__getitem__)

    def run(listed, day):
        (tmp_path / "releases").write_text(listed)
        monkeypatch.setattr(releases, "LIST", tmp_path / "releases")

        class Today(datetime.date):
            @classmethod
            def today(cls):
                return datetime.date.fromisoformat(day)

        monkeypatch.setattr(releases.datetime, "date", Today)
        status = releases.main(["check"])
        return status, capsys.readouterr().out
    return run


def test_up_to_date(check):
    assert check(FULL, "2026-11-06") == (0, "packaging/releases is up to date.\n")


def test_new_fedora(check):
    # The list 1.0.3 shipped with. Rawhide and the older 43 don't count.
    status, out = check(FULL.replace("fedora 45 -\n", ""), "2026-11-06")
    assert status == 1 and out == "Add `fedora 45 -` to packaging/releases: it's current or coming.\n"


def test_ubuntu_before_release(check):
    status, out = check(FULL, "2027-03-15")
    assert status == 1 and "`ubuntu 27.04 tenacious`" in out


def test_end_of_life(check):
    status, out = check(FULL, "2027-07-20")
    assert "`ubuntu-26.10` reached its end of life on 2027-07-15." in out
    assert "`fedora-44` reached its end of life on 2027-06-02." in out


def test_variants():
    names = [r["variant"] for r in releases.served()]
    assert names[0] == "arch" and "fedora-45" in names and len(names) == len(set(names))
    for name in names:
        assert (ROOT / "packaging" / "containers" / f"{name}.Containerfile").is_file(), name


def test_apply(tmp_path, monkeypatch):
    # June 2027: Fedora 46 has branched, Ubuntu 27.04 is out, Fedora 44 is over.
    bodhi = json.loads(json.dumps(BODHI))
    bodhi["releases"][-1]["branch"] = "f46"
    pages = {releases.BODHI: json.dumps(bodhi), releases.DISTRO_INFO.format("debian"): DEBIAN,
             releases.DISTRO_INFO.format("ubuntu"): UBUNTU}
    monkeypatch.setattr(releases, "fetch", pages.__getitem__)
    (tmp_path / "containers").mkdir()
    for build in ("fedora-44", "fedora-45", "ubuntu-26.10"):
        image = build.replace("-", ":")
        (tmp_path / "containers" / f"{build}.Containerfile").write_text(f"FROM docker.io/library/{image}@sha256:old\nRUN make\n")
    (tmp_path / "releases").write_text("# list\n" + FULL)
    monkeypatch.setattr(releases, "LIST", tmp_path / "releases")
    monkeypatch.setattr(releases, "CONTAINERS", tmp_path / "containers")
    today = datetime.date(2027, 6, 10)
    rows = releases.served()
    want, alive = releases.wanted(today)
    releases.apply(releases.missing(rows, want), releases.stale(rows, alive, today), resolve=lambda image: "sha256:new")
    assert (tmp_path / "releases").read_text().splitlines() == [
        "# list", "arch - -", "ubuntu 26.10 stonking", "ubuntu    27.04  tenacious", "debian 14 forky",
        "fedora 45 -", "fedora    46     -", "opensuse - -"]
    assert (tmp_path / "containers" / "fedora-46.Containerfile").read_text() == \
        "FROM docker.io/library/fedora:46@sha256:new\nRUN make\n"
    assert not (tmp_path / "containers" / "fedora-44.Containerfile").exists()
