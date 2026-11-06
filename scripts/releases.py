#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""releases.py variants [--json] | names | check | candidates | apply

Reads packaging/releases. "variants" lists its builds (arch, fedora-45),
as JSON for a workflow matrix with --json. "names" says them in words, as
the site and the README do. "check" asks the distributions
which releases are current or coming and says what the list lacks, or
still serves past its end of life; it exits 1 when the list needs a
change. "candidates" prints the missing releases and their images as
JSON, for the weekly builds that try them before we add them. "apply"
makes the change: the list's lines, and a Containerfile for each new
release, copied from its distribution's newest with the new image pinned
by digest. The docs that name the releases are left to a person.

Fedora's Bodhi lists a release from the day it branches, months before it
ships, and Debian's distro-info data has Debian's and Ubuntu's dates.
"""

import csv
import datetime
import io
import json
import sys
import urllib.request
from pathlib import Path

NAMES = {"arch": "Arch Linux", "ubuntu": "Ubuntu", "debian": "Debian", "fedora": "Fedora",
         "opensuse": "openSUSE Tumbleweed"}
LIST = Path(__file__).resolve().parent.parent / "packaging" / "releases"
CONTAINERS = LIST.parent / "containers"
BODHI = "https://bodhi.fedoraproject.org/releases/?exclude_archived=True&rows_per_page=100"
DISTRO_INFO = "https://salsa.debian.org/debian/distro-info-data/-/raw/main/{}.csv"
# An Ubuntu release joins this long before its release date: about when
# its beta comes out.
UBUNTU_LEAD = datetime.timedelta(weeks=6)


def served():
    rows = []
    for line in LIST.read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            target, release, suite = line.split()
            rows.append({
                "target": target,
                "release": None if release == "-" else release,
                "suite": None if suite == "-" else suite,
                "variant": target if release == "-" else f"{target}-{release}",
            })
    return rows


def names(rows):
    """Arch Linux, Ubuntu 26.10, Debian 14, Fedora 44 and 45, and openSUSE Tumbleweed"""
    groups = {}
    for r in rows:
        groups.setdefault(r["target"], []).append(r["release"])
    said = [" ".join([NAMES[t]] + [" and ".join(v for v in versions if v)]).strip()
            for t, versions in groups.items()]
    return ", ".join(said[:-1]) + ", and " + said[-1] if len(said) > 1 else said[0]


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as reply:
        return reply.read().decode()


def table(name):
    return list(csv.DictReader(io.StringIO(fetch(DISTRO_INFO.format(name)))))


def date(text):
    return datetime.date.fromisoformat(text) if text else None


def wanted(today):
    """(target, release, suite, image) for every release the list should
    have, and {(target, release): end of life} for those it may keep."""
    want, alive = [], {}
    for r in json.loads(fetch(BODHI))["releases"]:
        # Rawhide is named after the next release but isn't one yet.
        if r["id_prefix"] == "FEDORA" and r["name"] == f"F{r['version']}" and r["branch"] != "rawhide":
            if r["state"] in ("current", "pending", "frozen"):
                want.append(("fedora", r["version"], None, f"docker.io/library/fedora:{r['version']}"))
                alive[("fedora", r["version"])] = date(r["eol"])
    debian = [r for r in table("debian") if r["version"] and date(r["created"]) <= today]
    testing = [r for r in debian if not r["release"]]
    if testing:
        r = testing[-1]
        want.append(("debian", r["version"], r["series"], f"docker.io/library/debian:{r['series']}"))
    alive.update({("debian", r["version"]): date(r["eol"]) for r in debian})
    ubuntu = [r for r in table("ubuntu") if r["release"] and date(r["release"]) - UBUNTU_LEAD <= today]
    if ubuntu:
        r = ubuntu[-1]
        version = r["version"].split()[0]
        want.append(("ubuntu", version, r["series"], f"docker.io/library/ubuntu:{version}"))
    alive.update({("ubuntu", r["version"].split()[0]): date(r["eol"]) for r in ubuntu})
    return want, alive


def missing(rows, want):
    have = {(r["target"], r["release"]) for r in rows}
    # Fedora keeps several releases going. We start from the oldest we serve.
    oldest = min((int(r["release"]) for r in rows if r["target"] == "fedora"), default=0)
    return [w for w in want if (w[0], w[1]) not in have and not (w[0] == "fedora" and int(w[1]) < oldest)]


def stale(rows, alive, today):
    """(row, why) for each listed release the distribution has let go."""
    gone = []
    for r in rows:
        if r["release"] is None:
            continue
        key = (r["target"], r["release"])
        if key not in alive:
            gone.append((r, "is no longer current at the distribution"))
        elif alive[key] and alive[key] < today:
            gone.append((r, f"reached its end of life on {alive[key]}"))
    return gone


def digest(image):
    """The digest of a docker.io/library image's tag, from Docker Hub."""
    repo, tag = image.removeprefix("docker.io/").split(":")
    token = json.loads(fetch(f"https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:pull"))
    accept = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json"
    request = urllib.request.Request(f"https://registry-1.docker.io/v2/{repo}/manifests/{tag}", method="HEAD",
                                     headers={"Authorization": f"Bearer {token['token']}", "Accept": accept})
    with urllib.request.urlopen(request, timeout=60) as reply:
        return reply.headers["Docker-Content-Digest"]


def apply(new, gone, resolve=digest):
    lines = LIST.read_text().splitlines()
    for r, _ in gone:
        lines = [x for x in lines if x.startswith("#") or x.split()[:2] != [r["target"], r["release"] or "-"]]
        (CONTAINERS / f"{r['variant']}.Containerfile").unlink(missing_ok=True)
    for target, version, suite, image in new:
        def number(path):
            return [int(p) if p.isdigit() else 0 for p in path.stem.split("-", 1)[-1].split(".")]
        newest = max(CONTAINERS.glob(f"{target}-*.Containerfile"), key=number)
        recipe = newest.read_text().splitlines()
        recipe[0] = f"FROM {image}@{resolve(image)}"
        (CONTAINERS / f"{target}-{version}.Containerfile").write_text("\n".join(recipe) + "\n")
        at = max((i for i, x in enumerate(lines) if x.split()[:1] == [target]), default=len(lines) - 1)
        lines.insert(at + 1, f"{target:<10}{version:<7}{suite or '-'}")
    LIST.write_text("\n".join(lines) + "\n")


def main(args):
    rows = served()
    if args[:1] == ["variants"]:
        builds = [r["variant"] for r in rows]
        print(json.dumps(builds) if "--json" in args else "\n".join(builds))
        return 0
    if args[:1] == ["names"]:
        print(names(rows))
        return 0
    today = datetime.date.today()
    want, alive = wanted(today)
    new = missing(rows, want)
    if args[:1] == ["candidates"]:
        print(json.dumps([{"variant": f"{t}-{v}", "image": image} for t, v, _, image in new]))
        return 0
    gone = stale(rows, alive, today)
    if args[:1] == ["apply"]:
        apply(new, gone)
        return 0
    if args[:1] != ["check"]:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    report = [f"Add `{t} {v} {s or '-'}` to packaging/releases: it's current or coming." for t, v, s, _ in new]
    report += [f"`{r['variant']}` {why}." for r, why in gone]
    print("\n".join(report) or "packaging/releases is up to date.")
    return 1 if report else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
