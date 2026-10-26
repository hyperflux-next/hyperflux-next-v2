#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# repo-verify.sh SITE
#
# Checks a signed site the way a user's package manager would, trusting
# nothing but the public key in packaging/keys: every signature must be a
# good one by the project signing key, and every file must match the
# checksums the signed indexes give for it.

set -euo pipefail
cd "$(dirname "$0")/.."
site=$(realpath "${1:?usage: repo-verify.sh SITE}")
# shellcheck source=packaging/repository.conf
. packaging/repository.conf
home=$(mktemp -d)
trap 'rm -rf "$home"' EXIT
export GNUPGHOME=$home/gnupg
install -d -m 0700 "$GNUPGHOME"
gpg --batch --quiet --import "$KEY_FILE"
fail() { echo "repo-verify: $*" >&2; exit 1; }

verify() { # signature [data]
    gpg --batch --status-fd 1 --verify "$@" 2>/dev/null | grep -q "^\[GNUPG:\] VALIDSIG $SIGNING_KEY " ||
        fail "$1 isn't a good signature by $SIGNING_KEY"
}

cmp -s "$KEY_FILE" "$site/hyperflux-next-repository-key.asc" || fail "the site has another public key"
verify "$site/SHA256SUMS.asc" "$site/SHA256SUMS"
(cd "$site" && sha256sum --quiet -c SHA256SUMS) || fail "SHA256SUMS doesn't match"

for package in "$site"/arch/*.pkg.tar.zst "$site"/arch/hyperflux-next.db "$site"/arch/hyperflux-next.files; do
    verify "$package.sig" "$package"
done

for dists in "$site"/ubuntu/dists/* "$site"/debian/dists/*; do
    verify "$dists/InRelease"
    verify "$dists/Release.gpg" "$dists/Release"
    # Release vouches for Packages, Packages for every .deb.
    awk '/^SHA256:/ {on = 1; next} /^[^ ]/ {on = 0} on {print $1 "  " $3}' "$dists/Release" |
        (cd "$dists" && sha256sum --quiet -c) || fail "$dists/Release doesn't match its indexes"
    awk '/^Filename:/ {f = $2} /^SHA256:/ {print $2 "  " f}' "$dists/main/binary-amd64/Packages" |
        (cd "$dists/../.." && sha256sum --quiet -c) || fail "a package in $dists doesn't match Packages"
done

rpmdb=$home/rpm
mkdir -p "$rpmdb"
rpmkeys --dbpath "$rpmdb" --import "$KEY_FILE"
for repo in "$site"/fedora/*/x86_64 "$site"/opensuse/*/x86_64; do
    verify "$repo/repodata/repomd.xml.asc" "$repo/repodata/repomd.xml"
    for rpm in "$repo"/packages/*.rpm; do
        rpmkeys --dbpath "$rpmdb" --checksig "$rpm" | grep -q ': digests signatures OK$' ||
            fail "$rpm isn't signed by the project key"
    done
done
echo "repo-verify: $site is signed by $SIGNING_KEY throughout"
