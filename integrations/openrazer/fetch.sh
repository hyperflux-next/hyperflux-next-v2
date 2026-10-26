#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# fetch.sh DIR
#
# Downloads OpenRazer at the commit in PIN, checks the archive against the
# pinned SHA-256, applies patches/*.patch and leaves the tree in DIR.
# Running it again with the same pin and patches does nothing.
#
# OPENRAZER_ARCHIVE=FILE uses an archive that is already on disk (package
# builds that download sources themselves). It is checked the same way.

set -eu

here=$(cd "$(dirname "$0")" && pwd)
dest=${1:?usage: fetch.sh DIR}

# shellcheck disable=SC1091 # plain KEY=value lines
. "$here/PIN"

patches=$(find "$here/patches" -name '*.patch' | LC_ALL=C sort)
if [ -z "$patches" ]; then
    echo "fetch.sh: no patches in $here/patches" >&2
    exit 1
fi
# The stamp names the commit and the exact patch contents: editing a patch
# refetches instead of keeping a stale tree.
# shellcheck disable=SC2086 # one path per word
stamp="$OPENRAZER_COMMIT $(cat $patches | sha256sum | cut -d' ' -f1)"
if [ -f "$dest/.hyperflux-next-pin" ]; then
    if [ "$(cat "$dest/.hyperflux-next-pin")" = "$stamp" ]; then
        exit 0
    fi
elif [ -e "$dest" ]; then
    echo "fetch.sh: $dest exists and is not a tree this script made" >&2
    exit 1
fi

# Work next to DIR so the finished tree can be renamed into place.
mkdir -p "$(dirname "$dest")"
work=$(mktemp -d "$(dirname "$dest")/.openrazer-fetch.XXXXXX")
trap 'rm -rf "$work"' EXIT

# A downloaded archive stays next to DIR: tests/check-patches.sh needs the
# pristine tree, and a changed patch shouldn't mean another download.
if [ -n "${OPENRAZER_ARCHIVE:-}" ]; then
    archive=$OPENRAZER_ARCHIVE
else
    archive=$(dirname "$dest")/openrazer-$OPENRAZER_COMMIT.tar.gz
    if [ ! -f "$archive" ]; then
        url=${OPENRAZER_REPO%.git}/archive/$OPENRAZER_COMMIT.tar.gz
        echo "Downloading $url"
        if command -v curl >/dev/null 2>&1; then
            curl -fsSL --retry 3 -o "$work/archive" "$url"
        else
            wget -q -O "$work/archive" "$url"
        fi
        mv "$work/archive" "$archive"
    fi
fi

actual=$(sha256sum "$archive" | cut -d' ' -f1)
if [ "$actual" != "$OPENRAZER_SHA256" ]; then
    echo "fetch.sh: $archive has SHA-256 $actual, expected $OPENRAZER_SHA256" >&2
    exit 1
fi

tar -xzf "$archive" -C "$work"
tree=$work/openrazer-$OPENRAZER_COMMIT
if [ ! -d "$tree" ]; then
    echo "fetch.sh: the archive does not contain openrazer-$OPENRAZER_COMMIT" >&2
    exit 1
fi

# No fuzz: a patch that doesn't apply exactly should fail here, not
# produce a quietly different tree.
for patch in $patches; do
    patch -d "$tree" -p1 --forward --fuzz=0 --no-backup-if-mismatch -s -i "$patch"
done
echo "$stamp" >"$tree/.hyperflux-next-pin"

rm -rf "$dest"
mv "$tree" "$dest"
echo "OpenRazer $OPENRAZER_VERSION ($OPENRAZER_COMMIT) is in $dest"
