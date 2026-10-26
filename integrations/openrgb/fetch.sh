#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# fetch.sh DIR [6|5]
#
# Downloads the OpenRGB release pinned for the given Qt major version (6 is
# OpenRGB 1.1, 5 is OpenRGB 1.0), checks it against the pinned SHA-256,
# applies patches/*.patch and leaves the tree in DIR. Running it again with
# the same pin and patches does nothing.
#
# OPENRGB_ARCHIVE=FILE uses an archive that is already on disk (package
# builds that download sources themselves). It is checked the same way.

set -eu

here=$(cd "$(dirname "$0")" && pwd)
dest=${1:?usage: fetch.sh DIR [6|5]}
qt=${2:-6}

. "$here/PIN"
case $qt in
    6) commit=$OPENRGB_QT6_COMMIT sha256=$OPENRGB_QT6_SHA256 ;;
    5) commit=$OPENRGB_QT5_COMMIT sha256=$OPENRGB_QT5_SHA256 ;;
    *) echo "fetch.sh: Qt major version must be 5 or 6, not $qt" >&2; exit 2 ;;
esac

patches=$(find "$here/patches" -name '*.patch' | LC_ALL=C sort)
# The stamp names the commit and the exact patch contents. Editing a patch
# refetches instead of silently keeping a stale tree.
stamp="$commit $(cat $patches | sha256sum | cut -d' ' -f1)"
if [ -f "$dest/.hyperflux-next-pin" ] && [ "$(cat "$dest/.hyperflux-next-pin")" = "$stamp" ]; then
    exit 0
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/openrgb-fetch.XXXXXX")
trap 'rm -rf "$work"' EXIT

archive=${OPENRGB_ARCHIVE:-}
if [ -z "$archive" ]; then
    archive=$work/openrgb.tar.gz
    url=${OPENRGB_REPO%.git}/archive/$commit.tar.gz
    echo "Downloading $url"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$archive" "$url"
    else
        wget -q -O "$archive" "$url"
    fi
fi

actual=$(sha256sum "$archive" | cut -d' ' -f1)
if [ "$actual" != "$sha256" ]; then
    echo "fetch.sh: $archive has SHA-256 $actual, expected $sha256" >&2
    exit 1
fi

tar -xzf "$archive" -C "$work"
tree=$work/OpenRGB-$commit
if [ ! -d "$tree" ]; then
    echo "fetch.sh: the archive does not contain OpenRGB-$commit" >&2
    exit 1
fi

# No fuzz: a patch that doesn't apply exactly should fail here, not
# produce a quietly different tree.
for patch in $patches; do
    patch -d "$tree" -p1 --forward --fuzz=0 --no-backup-if-mismatch -s -i "$patch"
done
echo "$stamp" >"$tree/.hyperflux-next-pin"

rm -rf "$dest"
mkdir -p "$(dirname "$dest")"
mv "$tree" "$dest"
echo "OpenRGB $commit is in $dest"
