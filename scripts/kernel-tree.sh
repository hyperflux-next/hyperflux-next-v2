#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# kernel-tree.sh SERIES DIR
#
# Prepares a kernel tree that external modules can be built against:
# downloads the newest release of SERIES (6.1, 6.6, 6.12, ...; "latest" for
# the newest stable release, "mainline" for the newest release candidate)
# from kernel.org, configures it with defconfig and runs modules_prepare.
# There's no vmlinux. Build modules with KBUILD_MODPOST_WARN=1.

set -euo pipefail
series=${1:?usage: kernel-tree.sh SERIES DIR} dir=${2:?usage: kernel-tree.sh SERIES DIR}

read -r version source < <(curl -fsSL https://www.kernel.org/releases.json | python3 -c '
import json, sys
series, data = sys.argv[1], json.load(sys.stdin)
if series == "latest":
    wanted = [r for r in data["releases"] if r["version"] == data["latest_stable"]["version"]]
elif series == "mainline":
    wanted = [r for r in data["releases"] if r["moniker"] == "mainline"]
else:
    wanted = [r for r in data["releases"] if r["version"].startswith(series + ".")]
if not wanted:
    sys.exit(f"kernel.org lists no release for {series}")
print(wanted[0]["version"], wanted[0]["source"])
' "$series")
echo "Kernel $series is $version"

mkdir -p "$dir"
archive=$(mktemp)
trap 'rm -f "$archive"' EXIT
curl -fsSL --retry 3 -o "$archive" "$source"
case $source in
https://cdn.kernel.org/*)
    # The CDN can keep serving a sha256sums.asc from before the newest
    # release, so ask for a copy past its cache.
    sums=$(curl -fsSL -H 'Cache-Control: no-cache' "${source%/*}/sha256sums.asc?$(date +%s)")
    expected=$(awk -v f="${source##*/}" '$2 == f {print $1}' <<<"$sums")
    [ -n "$expected" ] || { echo "kernel.org's sha256sums.asc has no line for ${source##*/} yet" >&2; exit 1; }
    [ "$(sha256sum <"$archive" | cut -d' ' -f1)" = "$expected" ] || { echo "checksum mismatch for $source" >&2; exit 1; }
    ;;
esac
tar -xf "$archive" -C "$dir" --strip-components=1

make -C "$dir" -s defconfig
# Like distribution kernels: HID, USB HID, power supplies and uhid on, and
# CONFIG_WERROR off (W=1 warnings stay warnings here too).
"$dir/scripts/config" --file "$dir/.config" -e HID -e USB_HID -e POWER_SUPPLY -m UHID -d WERROR
make -C "$dir" -s olddefconfig
make -C "$dir" -s -j"$(nproc)" modules_prepare
echo "$version" >"$dir/.hyperflux-next-kernel"
