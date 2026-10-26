#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# check-patches.sh TREE
#
# Checks that patches/*.patch also apply with git apply, which is stricter
# about the patch format than patch(1), to the pristine pinned archive,
# that TREE is exactly that archive with the patches applied, and that
# each driver asks whether a device is on USB before treating it as one.

set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
tree=$(cd "${1:?usage: check-patches.sh TREE}" && pwd)
# shellcheck disable=SC1091 # plain KEY=value lines
. "$here/PIN"

archive=${OPENRAZER_ARCHIVE:-$(dirname "$tree")/openrazer-$OPENRAZER_COMMIT.tar.gz}
if [ ! -f "$archive" ]; then
    echo "check-patches.sh: $archive is missing; fetch.sh keeps it after downloading" >&2
    exit 1
fi
actual=$(sha256sum "$archive" | cut -d' ' -f1)
if [ "$actual" != "$OPENRAZER_SHA256" ]; then
    echo "check-patches.sh: $archive has SHA-256 $actual, expected $OPENRAZER_SHA256" >&2
    exit 1
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/openrazer-check.XXXXXX")
trap 'rm -rf "$work"' EXIT
tar -xzf "$archive" -C "$work"

patches=$(find "$here/patches" -name '*.patch' | LC_ALL=C sort)
# Inside a git checkout, git apply would take paths relative to that
# checkout's top and skip what lies outside the current directory.
export GIT_CEILING_DIRECTORIES="$work"
cd "$work/openrazer-$OPENRAZER_COMMIT"
# One at a time and in order: given several at once, git apply checks each
# against the pristine files.
for patch in $patches; do
    git apply --check "$patch"
    git apply "$patch"
done

diff -r -x .hyperflux-next-pin . "$tree"

# 0002: each probe, and razeraccessory's match callback, asks hid_is_usb()
# before it looks at the USB device behind its parent, so a new pin can't
# quietly undo that.
for check in razeraccessory:razer_accessory_match razeraccessory:razer_accessory_probe \
    razerkbd:razer_kbd_probe razerkraken:razer_kraken_probe razermouse:razer_mouse_probe; do
    awk -v fn="${check#*:}" '
        $0 ~ "^static [a-z]+ " fn "\\(" { inside = 1 }
        !inside { next }
        /hid_is_usb\(hdev\)/ { asked = 1 }
        /interface_to_usbdev|cur_altsetting|_init\(dev/ && !asked { exit 1 }
        /^}/ { exit !asked }
        END { exit !asked }
    ' "$tree/driver/${check%%:*}_driver.c" ||
        { echo "check-patches.sh: ${check#*:} uses the USB parent before hid_is_usb()" >&2; exit 1; }
done
echo "The patches apply to OpenRazer $OPENRAZER_VERSION and $tree matches"
