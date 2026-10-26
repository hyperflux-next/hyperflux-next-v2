#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# check-install.sh
#
# Installs this directory's openrazer-driver-dkms and full files into a
# scratch root, once for each OpenRazer group a distribution uses, and
# checks what lands where. Run it after `make openrazer-tree`. The default
# install paths are assumed.

set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck disable=SC1091 # plain KEY=value lines
. "$here/PIN"
cd "$here/../.."
dkms=usr/src/openrazer-driver-$OPENRAZER_VERSION

root=$(mktemp -d "${TMPDIR:-/tmp}/openrazer-install.XXXXXX")
trap 'rm -rf "$root"' EXIT

fail() {
    echo "check-install.sh: $*" >&2
    exit 1
}

LC_ALL=C sort >"$root/expected" <<EOF
usr/lib/modules-load.d/hyperflux-next-openrazer.conf
usr/lib/sysusers.d/openrazer.conf
usr/lib/udev/razer_mount
usr/lib/udev/rules.d/69-hyperflux-next-openrazer-access.rules
usr/lib/udev/rules.d/70-hyperflux-next-openrazer-keys.rules
usr/lib/udev/rules.d/99-razer.rules
$dkms/LICENSES/CC-BY-SA-4.0.txt
$dkms/LICENSES/GPL-2.0-or-later.txt
$dkms/Makefile
$dkms/dkms.conf
$dkms/driver/Makefile
$dkms/driver/razeraccessory_driver.c
$dkms/driver/razeraccessory_driver.h
$dkms/driver/razerchromacommon.c
$dkms/driver/razerchromacommon.h
$dkms/driver/razercommon.c
$dkms/driver/razercommon.h
$dkms/driver/razerkbd_driver.c
$dkms/driver/razerkbd_driver.h
$dkms/driver/razerkraken_driver.c
$dkms/driver/razerkraken_driver.h
$dkms/driver/razermouse_driver.c
$dkms/driver/razermouse_driver.h
$dkms/driver/usb_hid_keys.h
EOF

for group in openrazer plugdev; do
    dest=$root/$group
    # A clean MAKEFLAGS keeps the caller's path overrides out of the layout.
    MAKEFLAGS='' ${MAKE:-make} -s install-openrazer-dkms-tree install-openrazer-full \
        DESTDIR="$dest" OPENRAZER_GROUP="$group" >/dev/null

    (cd "$dest" && find . -type f | sed 's|^\./||' | LC_ALL=C sort) >"$root/found"
    diff -u "$root/expected" "$root/found" || fail "unexpected files for $group"

    rules=$dest/usr/lib/udev/rules.d/99-razer.rules
    mount=$dest/usr/lib/udev/razer_mount
    [ "$(grep -c "GROUP:=\"$group\"" "$rules")" = 1 ] || fail "99-razer.rules doesn't set $group"
    grep -q 'ENV{HID_PHYS}=="hyperflux-next/native/v1/\*"' "$rules" || fail "99-razer.rules has no bind rules"
    grep -q "chgrp -R $group " "$mount" || fail "razer_mount doesn't use $group"
    [ -x "$mount" ] || fail "razer_mount isn't executable"
    [ "$(grep -v '^#' "$dest/usr/lib/sysusers.d/openrazer.conf")" = "g $group -" ] ||
        fail "openrazer.conf doesn't create $group"
    if [ "$group" != plugdev ] && grep -q plugdev "$rules" "$mount"; then
        fail "plugdev is left in the $group files"
    fi
done
echo "The openrazer-driver-dkms and full files install as expected"
