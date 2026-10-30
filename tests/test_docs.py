# SPDX-License-Identifier: GPL-2.0-only
"""Facts the docs write down by hand agree with where they come from: the
OpenRazer and OpenRGB pins, the releases the repository serves, the signing
key, the device catalogue and the code's own constants. A change on either
side fails here until the other side says the same."""

import re
import shutil
import subprocess
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
USER_DOCS = ["README.md", "SUPPORT.md", "CONTRIBUTING.md", "man/hyperfluxctl.1",
             *sorted(str(p.relative_to(ROOT)) for p in (ROOT / "docs/user").glob("*.md"))]
WORDS = {"one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7, "eight": 8,
         "nine": 9, "ten": 10, "twice": 2, "a": 1}


def read(path):
    return (ROOT / path).read_text()


def assignments(path):
    return dict(re.findall(r"(?m)^(\w+)=(\S+)$", read(path)))


def const(path, name):
    """A Rust constant: a number, a sum or product of numbers, or a list of them."""
    expr = re.search(rf"\bconst {name}: [^=]+= (.+?);", read(path), re.S).group(1)
    numbers = [int(n.replace("_", "")) for n in re.findall(r"\d[\d_]*(?:\.\d+)?", expr) if "." not in n]
    if "[" in expr:
        return numbers
    if re.fullmatch(r"[\d_]+\.\d+", expr.strip()):
        return float(expr)
    value = 1 if "*" in expr else 0
    for n in numbers:
        value = value * n if "*" in expr else value + n
    return value


def literal(path, pattern):
    """Numbers caught by pattern's groups in a source file, as written there."""
    m = re.search(pattern, read(path))
    assert m, f"{path} no longer matches {pattern}"
    values = [int(g.replace("_", "")) for g in m.groups()]
    return values[0] if len(values) == 1 else values


def number(text):
    text = text.strip()
    if text.lower() in WORDS:
        return WORDS[text.lower()]
    found = [int(n) for n in re.findall(r"\d+", text)]
    return found[0] if len(found) == 1 else found


STALL = "crates/hyperfluxctl/src/stall.rs"
ARGS = "crates/hyperfluxctl/src/args.rs"
WIRE = "crates/hfx-bridge/src/wire.rs"
CHARGE = "crates/hfx-bridge/src/charge.rs"
MAN = "man/hyperfluxctl.1"
stale_min = const("crates/hfx-proto/src/present.rs", "BATTERY_STALE_AFTER_MS") // 60_000
refresh_min = const("crates/hfx-bridge/src/state.rs", "REFRESH_AFTER_MS") // 60_000
report_s = const("crates/hyperfluxctl/src/report.rs", "DEFAULT_SECONDS")
threshold_ms = literal(ARGS, r'"--threshold-ms", \(\d+, \d+\), (\d+)\)')

# (what, doc, pattern in the doc, value from the code). Every pattern has to
# match at least once, and every match has to equal the value.
FACTS = [
    ("stale after", "docs/user/limitations.md", r"turns stale after (\d+) minutes", stale_min),
    ("stale after", "docs/user/limitations.md", r"The (\d+) minutes are based on", stale_min),
    ("stale after", "docs/user/first-run.md", r"after (\d+) minutes\s+`hyperfluxctl battery`", stale_min),
    ("stale after", "docs/user/moving-from-2x.md", r"after (\d+) minutes\s+`hyperfluxctl`", stale_min),
    ("stale after", "docs/user/troubleshooting.md", r"A reading older than (\d+) minutes", stale_min),
    ("battery asked again", "docs/user/first-run.md", r"gone (\w+) minutes without reporting", refresh_min),
    ("battery asked again", "docs/user/limitations.md", r"has gone (\w+) minutes without reporting", refresh_min),
    ("battery asked again", "docs/user/limitations.md", r"up to (\w+) minutes later", refresh_min),
    ("battery asked again", "docs/user/hardware.md", r"every (\w+) minutes unless", refresh_min),
    ("mat put down", "docs/user/limitations.md", r"and (\d+) s and \d+ s after you",
     const(CHARGE, "PUT_DOWN_AFTER_MS") // 1000),
    ("mat checked again", "docs/user/limitations.md", r"and \d+ s and (\d+) s after you",
     (const(CHARGE, "PUT_DOWN_AFTER_MS") + const(CHARGE, "RECHECK_MS")) // 1000),
    ("activity events", "docs/user/privacy.md", r"one activity event per (\d+) ms",
     const(WIRE, "ACTIVITY_INTERVAL_MS")),
    ("vendor report head", "docs/user/privacy.md", r"as its first (\d+) bytes",
     const("crates/hfx-bridge/src/ipc.rs", "VENDOR_HEAD")),
    ("device missing", "docs/user/troubleshooting.md", r"still missing after (\d+) seconds",
     literal("crates/hfx-proto/src/errors.rs", r"doesn't show up within (\d+) seconds")),
    ("stall threshold", "docs/user/troubleshooting.md", r"every gap over (\d+) ms", threshold_ms),
    ("stall threshold", MAN, r"threshold \(default (\d+) ms\)", threshold_ms),
    ("capture seconds", MAN, r"seconds \(default (\d+)\) into a JSON", literal(ARGS, r'let seconds = options\.number\("--seconds", \(1, 3600\), (\d+)\)')),
    ("stall-check seconds", MAN, r"seconds \(default (\d+)\) while you move", literal(ARGS, r'seconds: options\.number\("--seconds", \(10, 600\), (\d+)\)')),
    ("report seconds", MAN, r"seconds \(default (\d+):", report_s),
    ("report seconds", MAN, r"seconds \(default (\d+):", literal(ARGS, r'seconds: options\.number\("--seconds", \(1, 1800\), (\d+)\)')),
    ("report bench runs", MAN, r"bench \\-\\-defaults \\-\\-iterations (\d+)",
     const("crates/hyperfluxctl/src/report.rs", "BENCH_ITERATIONS")),
    ("bench first waits", MAN, r"\(default ([\d ]+) microseconds,", const(ARGS, "DEFAULT_DELAYS_US")),
    ("bench runs", MAN, r"microseconds, (\d+) runs each\)", literal(ARGS, r'"--iterations", \(\d+, \d+\), (\d+)\)')),
    ("bench runs range", MAN, r"number of runs, from (\d+ to \d+)\.", literal(ARGS, r'"--iterations", \((\d+), (\d+)\)')),
    ("reread first waits", MAN, r"Its defaults are first waits of ([\d ]+) and retry", const(ARGS, "REREAD_DELAYS_US")),
    ("reread intervals", MAN, r"retry\s+intervals of ([\d\s]+) microseconds", const(ARGS, "REREAD_RETRIES_US")),
    ("quick", MAN, r"cuts that to first waits of ([\d ]+) and retry intervals of (\d+\s+and\s+\d+)\.",
     const(ARGS, "QUICK")),
    ("reread interval", MAN, r"instead of the (\d+) ms the bridge uses", const(WIRE, "REREAD_INTERVAL_US") // 1000),
    ("reread interval", MAN, r"first wait and (\d+) ms between reads", const(WIRE, "REREAD_INTERVAL_US") // 1000),
    ("read window", MAN, r"the reads end (\d+) ms after the first",
     const(WIRE, "GET_INTERVAL_US") * (const(WIRE, "MAX_GETS") - 1) // 1000),
    ("inventory kept", MAN, r"keeps the first (\d+) bytes", const(WIRE, "INVENTORY_SHOWN")),
    ("vendor report head", MAN, r"up to (\d+) bytes each", const("crates/hfx-bridge/src/ipc.rs", "VENDOR_HEAD")),
    ("stall context reports", MAN, r"at least (\d+) motion reports", const(STALL, "CONTEXT_REPORTS")),
    ("stall context", MAN, r"in the\s+(\d+) ms before it", const(STALL, "CONTEXT_MS")),
    ("stall speed ratio", MAN, r"at most (\d+) times the other", int(const(STALL, "SPEED_RATIO"))),
    ("stall pause", MAN, r"Gaps over (\d+) s are pauses", const(STALL, "PAUSE_MS") // 1000),
    ("stall exchange", MAN, r"and (\d+) ms after its end", const(STALL, "EXCHANGE_MS")),
    ("stall kernel lines", MAN, r"within (\d+) ms of it", const(STALL, "KERNEL_SLACK_MS")),
    ("stall verdict", MAN, r"at least (\w+) as\s+many as chance", int(const(STALL, "LINK_FACTOR"))),
    ("ab query", MAN, r"every (\d+) ms through", const(STALL, "QUERY_EVERY_MS")),
    ("ab significance", MAN, r"is below (\d+)%", round(const(STALL, "AB_SIGNIFICANCE") * 100)),
    ("ab movement", MAN, r"Less than (\d+) s of movement", const(STALL, "MIN_MOVING_MS") // 1000),
]


@pytest.mark.parametrize("what, doc, pattern, value", FACTS, ids=[f"{f[1]}: {f[0]}" for f in FACTS])
def test_doc_numbers(what, doc, pattern, value):
    found = re.findall(pattern, read(doc))
    assert found, f"{doc} no longer says this ({pattern}); update FACTS"
    for match in found:
        got = number(" ".join(match) if isinstance(match, tuple) else match)
        want = [n for v in value for n in (v if isinstance(v, list) else [v])] if isinstance(value, list) else value
        assert got == want, f"{doc} says {got} for {what}, the code says {want}"


def test_openrazer_versions():
    pinned = assignments("integrations/openrazer/PIN")["OPENRAZER_VERSION"]
    # What Ubuntu 26.04 LTS and Debian 13 package, named in limitations.md.
    distributions = {"3.11.0", "3.10.2"}
    for doc in USER_DOCS:
        for version in re.findall(r"\b3\.\d{1,2}\.\d{1,2}\b", read(doc)):
            assert version in {pinned} | distributions, f"{doc} names OpenRazer {version}, the pin is {pinned}"
    for doc in ("README.md", "docs/user/hardware.md"):
        assert re.search(rf"OpenRazer\s+{re.escape(pinned)}", read(doc)), doc


def test_releases():
    conf = assignments("packaging/repository.conf")
    ubuntu, debian, fedora = conf["UBUNTU_RELEASE"], conf["DEBIAN_RELEASE"], conf["FEDORA_RELEASE"]
    # The unsupported releases the docs name on purpose.
    allowed = {"Ubuntu": {ubuntu, "26.04"}, "Debian": {debian, "13"}, "Fedora": {fedora, "43"}}
    for doc in USER_DOCS:
        text = read(doc)
        for name, versions in allowed.items():
            for version in re.findall(rf"\b{name}\s+(\d+(?:\.\d+)?)\b", text):
                assert version in versions, f"{doc} names {name} {version}"
        for version, suite in re.findall(r"\bUbuntu\s+(\d\d\.\d\d)\s+\((\w+)\)", text):
            assert (version, suite) == (ubuntu, conf["UBUNTU_SUITE"]), doc
        for version, suite in re.findall(r"\bDebian\s+(\d+)\s+\((\w+)\)", text):
            assert version != debian or suite == conf["DEBIAN_SUITE"], doc
        for version in re.findall(r"Ubuntu supports (\d\d\.\d\d) until", text):
            assert version == ubuntu, f"{doc}: the support date belongs to Ubuntu {version}"
    readme = read("README.md")
    for release in (f"Ubuntu {ubuntu}", f"Debian {debian}", f"Fedora {fedora}"):
        assert release in readme, release


def test_openrgb():
    pins = assignments("integrations/openrgb/PIN")
    versions = {pins["OPENRGB_QT6_VERSION"], pins["OPENRGB_QT5_VERSION"]}
    files = read("scripts/install-test.sh")
    for doc in USER_DOCS:
        text = read(doc)
        for version in re.findall(r"\bOpenRGB\s+(\d+\.\d+)(?![.\d])", text):
            assert version in versions, f"{doc} names OpenRGB {version}"
        # The openrgb.org files the guides name are the ones the install
        # tests download and use.
        for name in re.findall(r"\b((?:openrgb|libhidapi-hotplug)[\w.-]*?\.(?:deb|rpm))\b", text):
            if "*" not in name:
                assert name in files, f"{doc} names {name}, which the install tests don't use"


def test_fingerprints():
    if not shutil.which("gpg"):
        pytest.skip("gpg is not installed")
    key = ROOT / "packaging/keys/hyperflux-next-repository-key.asc"
    listing = subprocess.run(["gpg", "--batch", "--with-colons", "--show-keys", key],
                             capture_output=True, text=True, check=True).stdout
    fingerprints = set(re.findall(r"(?m)^fpr:+(\w{40}):", listing))
    seen = 0
    for doc in [*USER_DOCS, "CHANGELOG.md", "docs/maintainer/signing.md", "docs/maintainer/release.md"]:
        if not (ROOT / doc).exists():
            continue
        for spaced in re.findall(r"\b(?:[0-9A-F]{4}\s+){9}[0-9A-F]{4}\b", read(doc)):
            assert re.sub(r"\s+", "", spaced) in fingerprints, f"{doc} shows {spaced}"
            seen += 1
    assert seen
    notes = re.search(r"(?ms)^## \S+\n(.*?)(?=^## |\Z)", read("CHANGELOG.md")).group(1)
    shown = {re.sub(r"\s+", "", f) for f in re.findall(r"\b(?:[0-9A-F]{4}\s+){9}[0-9A-F]{4}\b", notes)}
    assert shown == fingerprints, "the newest release notes must end with both fingerprints"


def hardware_rows():
    """(name, product id, tier, apps) for every row of the hardware table."""
    return re.findall(r"(?m)^\| ([^|]+?) \| (?:mouse|keyboard) \| `1532:(\w{4})` \| `1532:\w{4}` \| (\w+) \|\s*([^|]*?)\s*\|$",
                      read("docs/user/hardware.md"))


def test_readme_devices():
    rows = hardware_rows()
    readme = read("README.md")
    tested = re.search(r"I test with (.+?) on the\s+receiver\.", readme, re.S).group(1)
    named = {re.sub(r"^(the )?Razer\s+", "", n.strip()).replace("\n", " ")
             for n in re.split(r",\s+|\s+and\s+", tested)}
    assert named == {name for name, _, tier, _ in rows if tier == "tested"}
    block = re.search(r"These are checked:\n\n((?:- .+\n)+)", readme).group(1)
    suffix = {"OpenRazer, OpenRGB": "", "OpenRazer": ", in OpenRazer only", "OpenRGB": ", in OpenRGB only",
              "OpenRGB 1.1": ", in OpenRGB 1.1 only"}
    want = [f"- {name}{suffix[apps]}" for name, _, tier, apps in rows if tier == "checked"]
    assert block.strip().split("\n") == want


def test_upstream_names():
    catalog = {f"{d['product_id']:04x}": d["name"].removeprefix("Razer ")
               for d in tomllib.loads(read("devices.toml"))["device"]}
    differ = [catalog[pid] for name, pid, _, _ in hardware_rows() if catalog[pid] != name]
    listed = re.search(r"These differ from the table: (.+?)\.\n", read("docs/user/hardware.md"), re.S).group(1)
    assert sorted(re.split(r",\s+|\s+and\s+", listed.replace("\n", " "))) == sorted(differ)


def test_site_address():
    site = re.search(r"(?m)^SITE_URL\s*[:?]?=\s*(\S+)", read("Makefile")).group(1)
    for doc in [*USER_DOCS, "CHANGELOG.md"]:
        for url in re.findall(r"https://[\w.-]+\.github\.io/[^\s)`>\"]*", read(doc)):
            assert url.startswith(site), f"{doc} links {url}"
