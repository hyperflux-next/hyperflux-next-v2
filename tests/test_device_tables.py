# SPDX-License-Identifier: GPL-2.0-only
"""The copies of the device facts agree with devices.toml.

The Rust table is always checked. An OpenRGB or OpenRazer copy whose file
is missing is skipped. ARCHITECTURE.md ("Device facts") describes the
format each copy has to keep.
"""

import re
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
CATALOG = tomllib.loads((ROOT / "devices.toml").read_text())
DEVICES = {d["product_id"]: d for d in CATALOG["device"]}
RUST_DEVICES = ROOT / "crates/hfx-proto/src/devices.rs"
RUST_OPENRAZER = ROOT / "crates/hfx-proto/src/openrazer.rs"


def text_or_skip(relative):
    path = ROOT / relative
    if not path.exists():
        pytest.skip(f"{relative} is missing")
    return path.read_text()


def parse_commands(strings):
    commands = {}
    for entry in strings:
        cls, ident, sizes = entry.split(":")
        commands[(int(cls, 16), int(ident, 16))] = [int(s) for s in sizes.split(",")]
    return commands


def rust_string(value):
    assert value.startswith('"') and value.endswith('"'), value
    return value[1:-1].replace("\\\\", "\\")


def rust_option(value):
    return None if value == "None" else rust_string(value.removeprefix("Some(").removesuffix(")"))


def rust_u8_lists(source):
    lists = {}
    for name, body in re.findall(r"const (\w+): &\[u8\] = &\[(.*?)\];", source, re.S):
        lists[name] = [int(n) for n in re.findall(r"\d+", body)]
    return lists


def rust_commands(text, constants):
    commands = {}
    for cls, ident, sizes in re.findall(
        r"cmd\(\s*0x([0-9a-f]{2}),\s*0x([0-9a-f]{2}),\s*(&\[[^\]]*\]|\w+)\s*\)", text
    ):
        values = [int(n) for n in re.findall(r"\d+", sizes)] if sizes.startswith("&[") else constants[sizes]
        commands[(int(cls, 16), int(ident, 16))] = values
    return commands


def rust_devices():
    """Parse the DEVICES literal. Brittle by design: the table is kept in a
    fixed layout so this stays a few regular expressions."""
    source = RUST_DEVICES.read_text()
    constants = rust_u8_lists(source)
    shared = {
        name: rust_commands(body, constants)
        for name, body in re.findall(r"const (\w+): &\[Command\] = &\[(.*?)\n\];", source, re.S)
    }
    table = source.split("pub const DEVICES: &[Device] = &[", 1)[1]
    table = table.split("\n];", 1)[0]
    devices = {}
    for block in re.split(r"\n    Device \{", table)[1:]:
        # Undo rustfmt's line wrapping of long string values.
        block = re.sub(r'Some\(\s*("[^"]*")\s*,?\s*\)', r"Some(\1)", block)
        block = re.sub(r':\n\s+"', ': "', block)
        fields = dict(re.findall(r"^        (\w+): (.+?),?$", block, re.M))
        device = {
            "product_id": int(fields["product_id"], 16),
            "wired_product_id": int(fields["wired_product_id"], 16),
            "kind": {"Kind::Mouse": "mouse", "Kind::Keyboard": "keyboard"}[fields["kind"]],
            "name": rust_string(fields["name"]),
            "tier": {
                "Tier::Tested": "tested",
                "Tier::Checked": "checked",
                "Tier::Listed": "listed",
            }[fields["tier"]],
            "charge": {
                "ChargeDecoder::MouseStatus": "mouse-status",
                "ChargeDecoder::KeyboardStatus": "keyboard-status",
                "ChargeDecoder::None": "none",
            }[fields["charge"]],
            "first_get_delay_us": int(fields["first_get_delay_us"].replace("_", "")),
        }
        listed = fields["commands"]
        device["commands"] = shared[listed] if listed in shared else rust_commands(block, constants)
        if "openrazer: Some(OpenRazer {" in block:
            inner = dict(re.findall(r"^            (\w+): (.+?),$", block, re.M))
            device["openrazer"] = {
                "driver": rust_string(inner["driver"]),
                "class": rust_string(inner["class"]),
                "event_file_regex": rust_option(inner["event_file_regex"]),
                "serial_transaction_id": int(inner["serial_transaction_id"], 16),
                "key_event_link": rust_option(inner["key_event_link"]),
            }
        else:
            assert "openrazer: None" in block, block
        devices[device["product_id"]] = device
    return devices


def test_rust_receiver():
    source = RUST_DEVICES.read_text()
    block = source.split("pub const RECEIVER: Receiver = Receiver {", 1)[1].split("};", 1)[0]
    fields = dict(re.findall(r"(\w+): (.+?),", block))
    receiver = CATALOG["receiver"]
    assert rust_string(fields["name"]) == receiver["name"]
    for key in ("vendor_id", "product_id", "pointer_lane_selector", "keyboard_lane_selector"):
        assert int(fields[key], 16) == receiver[key], key


def test_rust_devices():
    rust = rust_devices()
    assert sorted(rust) == sorted(DEVICES)
    for pid, expected in DEVICES.items():
        got = rust[pid]
        label = f"{pid:04x} {expected['name']}"
        for key in ("wired_product_id", "kind", "name", "tier", "charge", "first_get_delay_us"):
            assert got[key] == expected[key], f"{label}: {key}"
        assert got["commands"] == parse_commands(expected["commands"]), label
        want = expected.get("openrazer")
        if want is None:
            assert "openrazer" not in got, label
        else:
            want = dict(want)
            want.pop("key_link_overlaps", None)
            want.setdefault("event_file_regex", None)
            want.setdefault("key_event_link", None)
            assert got["openrazer"] == want, label


def test_rust_openrazer_modules():
    source = RUST_OPENRAZER.read_text()

    def names(constant):
        body = re.search(rf"pub const {constant}: &\[&str\] = &\[(.*?)\];", source, re.S)
        return re.findall(r'"([^"]+)"', body.group(1))

    assert names("MODULES") == CATALOG["openrazer"]["modules"]
    assert names("PATCHED_MODULES") == CATALOG["openrazer"]["patched_modules"]


def test_catalog_rules():
    for pid, device in DEVICES.items():
        label = f"{pid:04x}"
        assert device["name"].startswith("Razer "), label
        assert device["kind"] in ("mouse", "keyboard"), label
        assert (device["tier"] == "listed") == (not device["commands"]), label
        if device["charge"] != "none":
            assert device["tier"] == "tested", f"{label}: charge decoding needs hardware proof"
        # The range hyperfluxctl bench measures. Shorter than 6 ms needs a bench run.
        assert 1000 <= device["first_get_delay_us"] <= 8000, label
        if device["first_get_delay_us"] < 6000:
            assert device["tier"] == "tested", f"{label}: a short first-GET wait needs bench results"
        razer = device.get("openrazer")
        if razer:
            assert (razer.get("key_event_link") is not None) == (device["kind"] == "keyboard"), label
            assert razer["serial_transaction_id"] == (0x1F if device["kind"] == "mouse" else 0xFF)


def test_key_links_match_own_device():
    regexes = {
        pid: re.compile(d["openrazer"]["event_file_regex"])
        for pid, d in DEVICES.items()
        if "event_file_regex" in d.get("openrazer", {})
    }
    for pid, device in DEVICES.items():
        link = device.get("openrazer", {}).get("key_event_link")
        if link is None:
            continue
        name = f"hyperflux-next-pci-0000_00_14_0-usb-0_2_1_1-{link}"
        matches = [other for other, regex in regexes.items() if regex.match(name)]
        assert matches == [pid], f"{name} matches {[hex(m) for m in matches]}"


def test_openrgb_table():
    source = text_or_skip("integrations/openrgb/src/device_table.hpp")
    rows = re.findall(
        r'\{\s*0x([0-9a-fA-F]{4})\s*,\s*DeviceKind::(Mouse|Keyboard)\s*,\s*"([a-z0-9_]+)"\s*\}', source
    )
    got = {int(pid, 16): (kind.lower(), key) for pid, kind, key in rows}
    want = {
        pid: (d["kind"], d["openrgb"]["key"]) for pid, d in DEVICES.items() if "openrgb" in d
    }
    assert got == want


def test_openrazer_key_rules():
    rules = text_or_skip("integrations/openrazer/70-hyperflux-next-openrazer-keys.rules")
    links = re.findall(r'SYMLINK\+="input/by-id/hyperflux-next-\$env\{ID_PATH_TAG\}-([^"]+)"', rules)
    want = {
        d["openrazer"]["key_event_link"]
        for d in DEVICES.values()
        if d.get("openrazer", {}).get("key_event_link")
    }
    assert set(links) == want
    # One rule decides which node gets the links and uaccess: interface 1's
    # keyboard node, not its pointer or its third node.
    jumps = [line for line in rules.splitlines() if 'GOTO="hyperflux_next_openrazer_keys_link"' in line]
    assert len(jumps) == 1
    for match in (
        'ENV{ID_MODEL_ID}=="00cf"',
        'ENV{ID_USB_INTERFACE_NUM}=="01"',
        'ENV{ID_INPUT_KEYBOARD}=="1"',
    ):
        assert match in jumps[0], match
    link_section = rules.split('LABEL="hyperflux_next_openrazer_keys_link"')[1]
    assert 'TAG+="uaccess"' in link_section
    assert rules.count('TAG+="uaccess"') == 1


def test_openrazer_modules_load():
    text = text_or_skip("integrations/openrazer/hyperflux-next-openrazer.modules-load.conf")
    lines = [line.strip() for line in text.splitlines() if line.strip() and not line.startswith("#")]
    assert lines == CATALOG["openrazer"]["modules"] + ["uhid"]


def test_openrazer_patch_constants():
    patches = sorted((ROOT / "integrations/openrazer/patches").glob("*.patch"))
    if not patches:
        pytest.skip("no OpenRazer patch")
    text = "\n".join(p.read_text() for p in patches)
    source = RUST_OPENRAZER.read_text()
    for constant in ("PHYS_PREFIX", "CAPABILITY_NAME", "CAPABILITY_VALUE"):
        value = re.search(rf'pub const {constant}: &str = "([^"]+)";', source).group(1)
        assert value in text, constant
    assert "HFXNERR1" in text
    descriptor = re.search(r"pub const DESCRIPTOR: \[u8; 19\] = \[(.*?)\];", source, re.S).group(1)
    # Comments like "// Usage Page (0xff00)" would otherwise add bytes.
    descriptor = re.sub(r"//[^\n]*", "", descriptor)
    text = re.sub(r"//[^\n]*|/\*.*?\*/", "", text, flags=re.S)
    wanted = [int(b, 16) for b in re.findall(r"0x([0-9a-f]{2})\b", descriptor)]
    found = [int(b, 16) for b in re.findall(r"0x([0-9a-fA-F]{2})\b", text)]
    assert len(wanted) == 19
    assert any(found[i : i + len(wanted)] == wanted for i in range(len(found))), "descriptor bytes"
