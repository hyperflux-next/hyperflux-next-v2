#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""site-page.py VERSION SITE_URL PROJECT_URL

Prints the package site's front page: packaging/site/index.html with the
version, the addresses, the distributions in packaging/releases and the
devices in the hardware page's table filled in. repo-assemble.sh writes
it to the site.
"""

import html
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import releases  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE = ROOT / "packaging" / "site" / "index.html"
HARDWARE = ROOT / "docs" / "user" / "hardware.md"
# Which install guide covers which distribution.
GUIDES = {"arch": "install-arch.md", "ubuntu": "install-apt.md", "debian": "install-apt.md",
          "fedora": "install-rpm.md", "opensuse": "install-rpm.md"}
WORDS = {1: "One", 2: "Two", 3: "Three", 4: "Four", 5: "Five", 6: "Six", 7: "Seven", 8: "Eight",
         9: "Nine", 10: "Ten"}


def hardware_rows():
    """(name, tier, apps) for every row of the hardware table."""
    return re.findall(r"(?m)^\| ([^|]+?) \| (?:mouse|keyboard) \| `1532:\w{4}` \| `1532:\w{4}` \| (\w+) \|\s*([^|]*?)\s*\|$",
                      HARDWARE.read_text())


def device_rows(rows):
    out = []
    for name, tier, apps in rows:
        if tier == "listed":
            continue
        tested = ' class="tested"' if tier == "tested" else ""
        out.append(f'            <tr{tested}><td>{html.escape(name)}</td>'
                   f'<td class="status{" tested" if tested else ""}">{tier}</td>'
                   f'<td class="apps">{html.escape(apps)}</td></tr>')
    return "\n".join(out)


def listed_line(rows):
    count = sum(tier == "listed" for _, tier, _ in rows)
    if not count:
        return "Every device Razer lists is in the table."
    more = f"{WORDS.get(count, count)} more {'is' if count == 1 else 'are'}"
    return f"{more} on Razer's list but not driven by either yet."


def distro_rows(served, project_url):
    """One row per install guide, naming its distributions in list order."""
    guides = {}
    for row in served:
        versions = guides.setdefault(GUIDES[row["target"]], {}).setdefault(row["target"], [])
        if row["release"]:
            versions.append(row["release"])
    out = []
    for guide, targets in guides.items():
        names = ", ".join(" ".join([releases.NAMES[t], " and ".join(v)]).strip() for t, v in targets.items())
        out.append(f'            <li><a href="{project_url}/blob/main/docs/user/{guide}">'
                   f'<span class="os">{html.escape(names)}</span><span class="guide">guide</span></a></li>')
    return "\n".join(out)


def page(version, site_url, project_url):
    rows = hardware_rows()
    if not rows:
        sys.exit(f"site-page.py: no device table in {HARDWARE}")
    text = TEMPLATE.read_text()
    for marker, value in (("@DISTRO_ROWS@", distro_rows(releases.served(), project_url)),
                          ("@DEVICE_ROWS@", device_rows(rows)), ("@LISTED@", listed_line(rows)),
                          ("@VERSION@", version), ("@SITE_URL@", site_url),
                          ("@PROJECT_URL@", project_url)):
        text = text.replace(marker, value)
    left = re.findall(r"@[A-Z_]+@", text)
    if left:
        sys.exit(f"site-page.py: {TEMPLATE} has unknown markers: {', '.join(sorted(set(left)))}")
    return text


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__.strip().splitlines()[0])
    sys.stdout.write(page(*sys.argv[1:]))
