#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# repo-publish.sh SITE [--push]
#
# Commits a signed and verified site to the gh-pages branch, on top of what
# is published now, and shows the commit. With --push it also pushes the
# branch and uploads the signed packages and checksums to the draft
# GitHub release of the same version. Publishing the release itself stays
# a manual step (docs/maintainer/release.md).

set -euo pipefail
cd "$(dirname "$0")/.."
site=$(realpath "${1:?usage: repo-publish.sh SITE [--push]}")
push=${2:-}
# shellcheck source=packaging/repository.conf
. packaging/repository.conf
version=$(cat "$site/VERSION")

scripts/repo-verify.sh "$site"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if git ls-remote --exit-code --heads "$PAGES_REMOTE" gh-pages >/dev/null; then
    git clone --quiet --branch gh-pages --single-branch "$PAGES_REMOTE" "$work"
else
    git -C "$work" init --quiet --initial-branch gh-pages
    git -C "$work" remote add origin "$PAGES_REMOTE"
fi
find "$work" -mindepth 1 -maxdepth 1 ! -name .git -exec rm -rf {} +
cp -a "$site/." "$work/"
git -C "$work" add --all
# The identity is this checkout's, which may be set only for it. The commit
# isn't signed: GNUPGHOME holds the project key here, not a personal one.
# A site that's already published as it is gets no commit, which lets a run
# that stopped after pushing be repeated.
git -C "$work" diff --cached --quiet ||
    git -C "$work" -c user.name="$(git config user.name)" -c user.email="$(git config user.email)" \
        -c commit.gpgsign=false commit --quiet -m "Publish HyperFlux Next V2 $version"
git -C "$work" show --stat --format='%h %s' HEAD | tail -n 5

if [ "$push" != --push ]; then
    echo "Nothing pushed. Run again with --push to publish."
    exit 0
fi
git -C "$work" push origin HEAD:gh-pages

# Release assets are named <target>-<file>, since Ubuntu and Debian
# packages share file names.
assets=$(mktemp -d)
add() { cp "$2" "$assets/$1-${2##*/}"; }
for file in "$site"/arch/*-"$version"-*.pkg.tar.zst*; do add arch "$file"; done
for target in ubuntu debian; do
    for file in "$site/$target"/pool/main/h/hyperflux-next-v2/*_"$version"-*.deb; do add "$target" "$file"; done
done
for target in fedora opensuse; do
    for file in "$site/$target"/*/x86_64/packages/*-"$version"-*.rpm; do add "$target" "$file"; done
done
(cd "$assets" && sha256sum -- * >SHA256SUMS)
gpg --batch --yes --local-user "$SIGNING_KEY!" --armor --detach-sign --output "$assets/SHA256SUMS.asc" "$assets/SHA256SUMS"
gh release upload "v$version" --clobber "$assets"/*
rm -rf "$assets"
echo "Pushed gh-pages and uploaded the signed files to the v$version draft."
