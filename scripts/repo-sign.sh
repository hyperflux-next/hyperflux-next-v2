#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# repo-sign.sh SITE
#
# Signs the site that repo-assemble.sh laid out, on the maintainer's
# machine, with the project signing key from packaging/repository.conf.
# GnuPG uses whatever GNUPGHOME is set. This script never creates, imports
# or exports secret keys. Needs gpg, repo-add, rpmsign and podman or docker
# (createrepo_c runs in the Fedora container).

set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
site=$(realpath "${1:?usage: repo-sign.sh SITE}")
# shellcheck source=packaging/repository.conf
. packaging/repository.conf
version=$(cat "$site/VERSION")
engine=${CONTAINER_ENGINE:-$(command -v podman || command -v docker)}

case $SIGNING_KEY in
*[!0-9A-F]* | '') echo "repo-sign: set SIGNING_KEY in packaging/repository.conf first" >&2; exit 1 ;;
esac
[ -f "$KEY_FILE" ] || { echo "repo-sign: $KEY_FILE is missing" >&2; exit 1; }
gpg --batch --with-colons --show-keys "$KEY_FILE" | grep -q "^fpr:*:$SIGNING_KEY:" ||
    { echo "repo-sign: $KEY_FILE doesn't contain $SIGNING_KEY" >&2; exit 1; }
# The trailing ! makes GnuPG use exactly this (sub)key.
sign() { gpg --batch --yes --local-user "$SIGNING_KEY!" --digest-algo SHA512 "$@"; }

echo "== RPM"
"$engine" build -q -t hyperflux-next-build:fedora -f packaging/containers/fedora.Containerfile packaging/containers >/dev/null
for dir in "$site"/fedora/*/x86_64 "$site"/opensuse/*/x86_64; do
    rpmsign --addsign --define "_gpg_name $SIGNING_KEY!" --define '_gpg_digest_algo sha512' "$dir"/packages/*.rpm
    target=${dir#"$site"/}
    "$engine" run --rm -v "$site:/site" -w /site "hyperflux-next-build:fedora" bash -euc \
        "command -v createrepo_c >/dev/null || dnf install -y -q createrepo_c >/dev/null
         createrepo_c --quiet --no-database --general-compress-type gz $target"
    sign --armor --detach-sign --output "$dir/repodata/repomd.xml.asc" "$dir/repodata/repomd.xml"
done

echo "== APT"
for release in "$site"/ubuntu/dists/*/Release "$site"/debian/dists/*/Release; do
    sign --clearsign --output "${release%/Release}/InRelease" "$release"
    sign --armor --detach-sign --output "$release.gpg" "$release"
done

echo "== Arch"
cd "$site/arch"
for package in ./*.pkg.tar.zst; do
    [ -f "$package.sig" ] || sign --detach-sign --output "$package.sig" "$package"
done
rm -f hyperflux-next.db* hyperflux-next.files*
repo-add --quiet --include-sigs --sign --key "$SIGNING_KEY!" hyperflux-next.db.tar.zst ./*[-:]"$version"-*.pkg.tar.zst
# repo-add links the short names to the archives, but the site keeps real
# files so any web server serves them alike.
for name in db files; do
    rm -f "hyperflux-next.$name" "hyperflux-next.$name.sig"
    cp "hyperflux-next.$name.tar.zst" "hyperflux-next.$name"
    cp "hyperflux-next.$name.tar.zst.sig" "hyperflux-next.$name.sig"
done

echo "== Site"
cd "$site"
cp "$root/$KEY_FILE" hyperflux-next-repository-key.asc
find . -type f ! -name 'SHA256SUMS*' ! -name .nojekyll -printf '%P\n' | LC_ALL=C sort | xargs -d '\n' sha256sum >SHA256SUMS
sign --armor --detach-sign --output SHA256SUMS.asc SHA256SUMS
echo "Signed. Check it with scripts/repo-verify.sh $site"
