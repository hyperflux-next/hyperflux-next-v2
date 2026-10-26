#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Print the license notices of every crate compiled into the shipped
binaries: normal dependencies only, without test-only crates and without
proc-macro crates, which run at build time and leave no code behind.

Needs every crate in Cargo.lock on disk, as after cargo fetch."""

import json
import subprocess
import sys
from pathlib import Path


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


host = next(line.split()[1] for line in run("rustc", "-vV").splitlines() if line.startswith("host:"))
metadata = json.loads(run("cargo", "metadata", "--format-version", "1", "--locked",
                          "--offline", "--filter-platform", host))
packages = {p["id"]: p for p in metadata["packages"]}
nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
members = set(metadata["workspace_members"])
macros = {i for i, p in packages.items() if any("proc-macro" in t["kind"] for t in p["targets"])}

shipped, todo = set(), list(members)
while todo:
    node = nodes[todo.pop()]
    for dep in node["deps"]:
        normal = any(kind["kind"] is None for kind in dep["dep_kinds"])
        if normal and dep["pkg"] not in shipped | macros:
            shipped.add(dep["pkg"])
            todo.append(dep["pkg"])

out = sys.stdout
out.write("Third-party crates compiled into the HyperFlux Next V2 binaries.\n")
seen = {}
for package in sorted((packages[i] for i in shipped - members), key=lambda p: p["name"]):
    root = Path(package["manifest_path"]).parent
    texts = sorted(f for f in root.iterdir()
                   if f.is_file() and f.name.upper().startswith(("LICENSE", "COPYING", "NOTICE")))
    out.write(f"\n{'=' * 72}\n{package['name']} {package['version']} ({package['license']})\n")
    if not texts:
        sys.exit(f"crate-notices: {package['name']} has no license file")
    for text in texts:
        body = text.read_text(errors="replace").strip()
        if body in seen:
            out.write(f"\n--- {text.name}: the same text as {seen[body]}\n")
        else:
            seen[body] = f"{package['name']} {text.name}"
            out.write(f"\n--- {text.name}\n\n{body}\n")
