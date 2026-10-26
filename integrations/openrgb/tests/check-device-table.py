#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""check-device-table.py DEVICE_TABLE OPENRGB_TREE CATALOG [QT]

Every row of src/device_table.hpp must name a razer_device record in the
pinned OpenRGB sources with the same product id and device type. Rows
under `#if HFX_OPENRGB_QT >= 6` are for OpenRGB 1.1 only: with QT 5, the
OpenRGB 1.0 build, their records must be missing instead. Every
[openrgb] part of the catalog (devices.toml) must have that record's
transaction id, rows and columns, and allow the custom frames OpenRGB
sends: whole rows, 5 bytes of header and 3 per LED. A catalog device
without an [openrgb] part must have no record. tests/test_device_tables.py
holds the table to devices.toml.
"""

import re
import sys
import tomllib
from pathlib import Path

table = Path(sys.argv[1]).read_text()
qt = int(sys.argv[4]) if len(sys.argv) > 4 else 6
tree = Path(sys.argv[2]) / "Controllers/RazerController"
catalog = tomllib.loads(Path(sys.argv[3]).read_text())
pids = dict(re.findall(r"#define\s+(RAZER_\w+_PID)\s+0x([0-9A-Fa-f]{4})", (tree / "RazerDevices.h").read_text()))
records = {
    key: (int(pids.get(pid, "-1"), 16), kind, int(tx, 16), int(rows), int(columns))
    for key, pid, kind, tx, rows, columns in re.findall(
        r"razer_device (\w+) =\s*\{\s*\"[^\"]*\",\s*(\w+),\s*(\w+),\s*\w+,\s*(0x\w+),\s*(\d+),\s*(\d+),",
        (tree / "RazerDevices.cpp").read_text(),
    )
}

row = r'\{\s*0x([0-9a-fA-F]{4})\s*,\s*DeviceKind::(Mouse|Keyboard)\s*,\s*"([a-z0-9_]+)"\s*\}'
rows = re.findall(row, table)
newer = re.findall(row, "".join(re.findall(r"#if HFX_OPENRGB_QT >= 6\n(.*?)#endif", table, re.S)))
problems = []
for pid, kind, key in rows:
    if qt < 6 and (pid, kind, key) in newer:
        if key in records:
            problems.append(f"{key}: this OpenRGB has it, so the row needn't wait for 1.1")
    elif key not in records:
        problems.append(f"{key}: no such record")
    elif records[key][0] != int(pid, 16):
        problems.append(f"{key}: OpenRGB has {records[key][0]:04x}, not 0x{pid}")
    elif records[key][1] != f"DEVICE_TYPE_{kind.upper()}":
        problems.append(f"{key}: OpenRGB says {records[key][1]}, not {kind}")

for device in catalog["device"]:
    pid, part = device["product_id"], device.get("openrgb")
    label = f"{pid:04x} {device['name']}"
    if part is None:
        if any(record[0] == pid for record in records.values()):
            problems.append(f"{label}: OpenRGB drives it, but the catalog has no [openrgb] part")
        continue
    if part["key"] not in records:
        continue
    _, _, tx, height, width = records[part["key"]]
    if (part["transaction_id"], part["rows"], part["columns"]) != (tx, height, width):
        problems.append(f"{label}: OpenRGB has transaction id 0x{tx:02x}, {height} rows, {width} columns")
    frames = {s for c in device["commands"] if c.startswith("0f:03:") for s in c[6:].split(",")}
    if str(5 + 3 * width) not in frames:
        problems.append(f"{label}: 0f:03 lacks {5 + 3 * width}, OpenRGB's size for a row of {width}")

if not rows:
    problems.append("no rows found in the device table")
print("\n".join(problems) or f"{len(rows)} devices match {sys.argv[2]}")
sys.exit(1 if problems else 0)
