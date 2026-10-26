# SPDX-License-Identifier: GPL-2.0-only
"""devices.toml against the pinned OpenRazer and OpenRGB and the Razer pages
it cites.

The OpenRazer tests read the drivers' USB id tables, two of their
per-device switches and the daemon's classes; the patches add nothing
there but the virtual-bus entry. They skip when .upstreams/openrazer is
missing (make openrazer-tree fetches it). The OpenRGB side is
integrations/openrgb/tests/check-device-table.py, run here against each
pinned OpenRGB tree that is there. Razer's side is the [sources] snapshot in the
catalog, not the live pages.
"""

import ast
import re
import subprocess
import sys
import tomllib
import warnings
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
CATALOG = tomllib.loads((ROOT / "devices.toml").read_text())
DEVICES = CATALOG["device"]
OPENRAZER = ROOT / ".upstreams/openrazer"
PIN = dict(re.findall(r"(?m)^(\w+)=(\S+)$", (ROOT / "integrations/openrazer/PIN").read_text()
                      + (ROOT / "integrations/openrgb/PIN").read_text()))
POLL_READS = {"razer_chroma_misc_get_polling_rate(": (0x00, 0x85, 1),
              "razer_chroma_misc_get_polling_rate2(": (0x00, 0xC0, 1)}


def label(device):
    return f"{device['product_id']:04x} {device['name']}"


def commands(device):
    entries = (entry.split(":") for entry in device["commands"])
    return {(int(c, 16), int(i, 16), int(s)) for c, i, sizes in entries for s in sizes.split(",")}


def openrazer_tree():
    if not OPENRAZER.is_dir():
        pytest.skip(".upstreams/openrazer is missing")
    return OPENRAZER


def drivers():
    """Per driver: its source, the value of each pid macro, and the macros
    in its USB id table."""
    found = {}
    for name in ("razermouse", "razerkbd"):
        base = openrazer_tree() / "driver" / name
        source = Path(f"{base}_driver.c").read_text()
        header = Path(f"{base}_driver.h").read_text()
        values = {m: int(v, 16) for m, v in re.findall(r"#define\s+(USB_DEVICE_ID_\w+)\s+0x(\w{4})\b", header)}
        found[name] = source, values, set(re.findall(r"HID_USB_DEVICE\(USB_VENDOR_ID_RAZER,\s*(\w+)\)", source))
    return found


def branch(source, function, macro):
    """What a device runs in one of the driver's switch (usb_pid) functions:
    its case group and any it falls through to, or the default."""
    body = re.search(rf"\b{function}\(.*?\n}}\n", source, re.S).group(0)
    parts = re.split(r"((?:\s*(?:case\s+USB_DEVICE_ID_\w+|default)\s*:)+)", body)
    groups = list(zip(parts[1::2], parts[2::2]))
    start = next((i for i, (labels, _) in enumerate(groups) if re.search(rf"\b{macro}\s*:", labels)), None)
    if start is None:
        start = next((i for i, (labels, _) in enumerate(groups) if "default" in labels), len(groups))
    code = ""
    for _, part in groups[start:]:
        code += part
        if re.search(r"\bbreak;|\breturn\b", part):
            break
    return code


def daemon_classes():
    """The daemon's device classes with what they inherit inside one file."""
    classes = {}

    def methods(node):
        if isinstance(node, ast.BinOp):
            return methods(node.left) + methods(node.right)
        if isinstance(node, ast.Attribute):
            return classes[node.value.id]["methods"]
        return [e.value for e in node.elts]

    for path in sorted((openrazer_tree() / "daemon/openrazer_daemon/hardware").glob("*.py")):
        driver = {"mouse": "razermouse", "keyboards": "razerkbd"}.get(path.stem)
        for node in ast.parse(path.read_text()).body:
            if isinstance(node, ast.ClassDef):
                bases = [classes[b.id] for b in node.bases if isinstance(b, ast.Name) and b.id in classes]
                cls = dict(bases[0] if bases else {"methods": []}, driver=driver)
                for stmt in node.body:
                    if isinstance(stmt, ast.Assign) and isinstance(stmt.targets[0], ast.Name):
                        key, value = stmt.targets[0].id, stmt.value
                        if key == "USB_PID":
                            cls["pid"] = value.value
                        elif key == "EVENT_FILE_REGEX":
                            cls["regex"] = value.args[0].value
                        elif key == "MATRIX_DIMS":
                            cls["dims"] = [e.value for e in value.elts]
                        elif key == "METHODS":
                            cls["methods"] = methods(value)
                classes[node.name] = cls
    return classes


def test_openrazer_parts():
    found, classes = drivers(), daemon_classes()
    for device in DEVICES:
        pid, part, cited = device["product_id"], device.get("openrazer"), device["sources"].get("openrazer")
        tables = [d for d, (_, values, table) in found.items() if any(values.get(m) == pid for m in table)]
        named = [n for n, c in classes.items() if c.get("pid") == pid and c["driver"]]
        if part is None:
            assert not tables and not named and not cited, f"{label(device)}: OpenRazer drives it"
            continue
        version, driver, macro = cited.split()
        _, values, table = found[driver]
        assert version == PIN["OPENRAZER_VERSION"] and tables == [driver] == [part["driver"]], label(device)
        assert macro in table and values[macro] == pid, label(device)
        assert any(values.get(m) == device["wired_product_id"] for m in table), label(device)
        assert named == [part["class"]] and classes[part["class"]]["driver"] == driver, label(device)
        assert classes[part["class"]].get("regex") == part.get("event_file_regex"), label(device)


def test_openrazer_key_links():
    classes = daemon_classes()
    for device in DEVICES:
        part = device.get("openrazer", {})
        if "key_event_link" not in part:
            continue
        # The daemon matches the link's name. Any receiver's path tag will do.
        link = f"hyperflux-next-pci-0000_00_14_0-usb-0_2_1_1-{part['key_event_link']}"
        matching = {n for n, c in classes.items() if c.get("regex") and re.match(c["regex"], link)}
        assert part["class"] in matching, label(device)
        assert matching - {part["class"]} == set(part["key_link_overlaps"]), label(device)


def test_openrazer_commands():
    found, classes = drivers(), daemon_classes()
    for device in DEVICES:
        have, part = commands(device), device.get("openrazer")
        frames = {5 + 3 * device["openrgb"]["columns"]} if "openrgb" in device else set()
        methods = classes[part["class"]]["methods"] if part else []
        assert ((0x07, 0x80, 2) in have) == ("get_battery" in methods), f"{label(device)}: battery"
        assert ((0x07, 0x84, 2) in have) == ("is_charging" in methods), f"{label(device)}: charging"
        if part:
            source = found[part["driver"]][0]
            macro = device["sources"]["openrazer"].split()[2]
            read = branch(source, "razer_attr_read_poll_rate", macro)
            poll = {command for call, command in POLL_READS.items() if call in read}
            assert bool(poll & have) == ("get_poll_rate" in methods), f"{label(device)}: poll rate"
            frame = branch(source, "razer_attr_write_matrix_custom_frame", macro)
            if "set_key_row" in methods and re.search(r"custom_frame2\([^;]*,\s*0\)", frame):
                frames.add(5 + 3 * classes[part["class"]]["dims"][1])
            elif "set_key_row" in methods and "set_custom_frame(" in frame:
                frames.add(71)
        assert {s for c, i, s in have if (c, i) == (0x0F, 0x03)} == frames, f"{label(device)}: 0f:03"


@pytest.mark.parametrize("qt", ["6", "5"])
def test_openrgb_records(qt):
    tree = ROOT / f".upstreams/openrgb-{PIN[f'OPENRGB_QT{qt}_VERSION']}"
    if not tree.is_dir():
        pytest.skip(f"{tree.name} is missing")
    check = ROOT / "integrations/openrgb/tests/check-device-table.py"
    table = ROOT / "integrations/openrgb/src/device_table.hpp"
    result = subprocess.run([sys.executable, "-I", check, table, tree, ROOT / "devices.toml", qt],
                            capture_output=True, text=True)
    assert result.returncode == 0, result.stdout + result.stderr


def test_levels_and_sources():
    pages = CATALOG["sources"]
    for name, page in pages.items():
        assert page["url"].startswith("https://") and re.fullmatch(r"\d{4}-\d\d-\d\d", page["seen"]), name
    for device in DEVICES:
        sources, pid = device["sources"], device["product_id"]
        assert sources["razer"], label(device)
        for page in sources["razer"]:
            assert any(pid in pids for pids in pages[page]["lists"].values()), f"{label(device)}: {page}"
        if "openrgb" in device:
            assert sources["openrgb"] == f"{PIN['OPENRGB_QT6_VERSION']} {device['openrgb']['key']}", label(device)
        assert ("openrgb" in device) == ("openrgb" in sources), label(device)
        assert ("openrazer" in device) == ("openrazer" in sources), label(device)
        driven = "openrazer" in device or "openrgb" in device
        assert driven == (device["tier"] != "listed"), f"{label(device)}: {device['tier']}"
        if device["tier"] == "tested":
            assert sources["reports"] or sources.get("hardware"), f"{label(device)}: tested by whom?"


def test_razer_lists():
    known = {device["product_id"] for device in DEVICES}
    for name, page in CATALOG["sources"].items():
        for model, pids in page["lists"].items():
            for pid in sorted(set(pids) - known):
                warnings.warn(f"{name} lists the {model} ({pid:04x}), which devices.toml doesn't have")
