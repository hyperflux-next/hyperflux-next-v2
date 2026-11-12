#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# repo-assemble.sh SITE DIST [PREVIOUS]
#
# Lays out the unsigned package repository in SITE from the packages that
# scripts/package.sh left in DIST/<build>/, for every build in
# packaging/releases. PREVIOUS is a checkout of the published site. Its
# current release is carried over so people can go back one version. The
# index files are made with each distribution's own tools, in its
# container. scripts/repo-sign.sh signs the result.

set -euo pipefail
cd "$(dirname "$0")/.."
site=$(realpath -m "${1:?usage: repo-assemble.sh SITE DIST [PREVIOUS]}")
dist=$(realpath "${2:?usage: repo-assemble.sh SITE DIST [PREVIOUS]}")
previous=${3:+$(realpath "$3")}
releases=$(grep -v '^#' packaging/releases | grep .)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
site_url=$(sed -n 's/^SITE_URL ?= //p' Makefile)
engine=${CONTAINER_ENGINE:-$(command -v podman || command -v docker)}
in_image() { # build command
    "$engine" build -q -t "hyperflux-next-build:$1" -f "packaging/containers/$1.Containerfile" packaging/containers >/dev/null
    "$engine" run --rm -v "$site:/site" -w /site "hyperflux-next-build:$1" bash -euc "$2"
}
# Copies DIST/BUILD/PATTERN, plus the previous release's matching files
# from the same place in PREVIOUS, or from OLD_PLACE there.
collect() { # build pattern destination [old_place]
    mkdir -p "$3"
    # shellcheck disable=SC2086 # the pattern is meant to glob
    cp "$dist/$1"/$2 "$3/"
    if [ -n "$previous" ] && [ -f "$previous/VERSION" ]; then
        local old from=$previous/${3#"$site"/}
        old=$(cat "$previous/VERSION")
        [ -d "$from" ] || from=${4:+$previous/$4}
        [ "$old" = "$version" ] || [ -z "$from" ] || [ ! -d "$from" ] ||
            find "$from" -maxdepth 1 -name "*$old-[0-9]*" -exec cp {} "$3/" \;
    fi
}

rm -rf "$site"
mkdir -p "$site"
echo "$version" >"$site/VERSION"

collect arch '*.pkg.tar.zst' "$site/arch"
in_image arch "cd arch && repo-add -q hyperflux-next.db.tar.zst ./*-$version-*.pkg.tar.zst \
    && rm hyperflux-next.db hyperflux-next.files \
    && cp hyperflux-next.db.tar.zst hyperflux-next.db && cp hyperflux-next.files.tar.zst hyperflux-next.files"

# Each suite has its own pool, since the same version builds to the same
# file names for every Ubuntu or Debian release. Release is written next to
# the suite and moved in: written in place, the directory apt-ftparchive
# hashes would hold half of Release itself.
while read -r target release suite; do
    case $target in ubuntu) name=Ubuntu ;; debian) name=Debian ;; *) continue ;; esac
    # Before 1.0.4, one pool held the only suite.
    collect "$target-$release" '*.deb' "$site/$target/pool/$suite/main/h/hyperflux-next-v2" "$target/pool/main/h/hyperflux-next-v2"
    in_image "$target-$release" "apt-get update -qq && apt-get install -y -qq apt-utils >/dev/null
        cd $target && mkdir -p dists/$suite/main/binary-amd64
        apt-ftparchive --arch amd64 packages pool/$suite >dists/$suite/main/binary-amd64/Packages
        gzip -9n <dists/$suite/main/binary-amd64/Packages >dists/$suite/main/binary-amd64/Packages.gz
        apt-ftparchive -o APT::FTPArchive::Release::Origin='HyperFlux Next V2' \
            -o APT::FTPArchive::Release::Label='HyperFlux Next V2' -o APT::FTPArchive::Release::Suite=$suite \
            -o APT::FTPArchive::Release::Codename=$suite -o APT::FTPArchive::Release::Architectures=amd64 \
            -o APT::FTPArchive::Release::Components=main \
            -o APT::FTPArchive::Release::Description='HyperFlux Next V2 packages for $name' \
            release dists/$suite >dists/$suite.Release && mv dists/$suite.Release dists/$suite/Release"
    printf '%s\n' 'Types: deb' "URIs: $site_url/$target" "Suites: $suite" 'Components: main' \
        'Architectures: amd64' 'Signed-By: /etc/apt/keyrings/hyperflux-next.asc' >"$site/$target/$suite.sources"
    # The guides' file, for the newest suite: the list has it last.
    cp "$site/$target/$suite.sources" "$site/$target/hyperflux-next.sources"
done <<<"$releases"

while read -r target release _; do
    case $target in
    fedora) build=fedora-$release dir=fedora/$release ;;
    opensuse) build=opensuse dir=opensuse/tumbleweed ;;
    *) continue ;;
    esac
    collect "$build" '*.rpm' "$site/$dir/x86_64/packages"
    in_image "$build" "command -v createrepo_c >/dev/null || dnf install -y -q createrepo_c >/dev/null 2>&1 \
        || zypper --non-interactive -q install createrepo_c >/dev/null
        createrepo_c --quiet --no-database --general-compress-type gz $dir/x86_64"
done <<<"$releases"
# dnf fills in the release. One the site doesn't have yet is skipped, not
# an error that stops every other update on the system.
printf '%s\n' '[hyperflux-next]' 'name=HyperFlux Next V2 for Fedora' "baseurl=$site_url/fedora/\$releasever/\$basearch/" \
    'type=rpm-md' 'enabled=1' 'gpgcheck=1' 'repo_gpgcheck=1' 'skip_if_unavailable=1' \
    "gpgkey=$site_url/hyperflux-next-repository-key.asc" >"$site/fedora/hyperflux-next.repo"
printf '%s\n' '[hyperflux-next]' 'name=HyperFlux Next V2 for openSUSE' "baseurl=$site_url/opensuse/tumbleweed/\$basearch/" \
    'type=rpm-md' 'enabled=1' 'gpgcheck=1' 'repo_gpgcheck=1' "gpgkey=$site_url/hyperflux-next-repository-key.asc" \
    'autorefresh=1' >"$site/opensuse/hyperflux-next.repo"
# install.sh reads which releases there are from here.
echo "$releases" >"$site/releases"

project_url=$(sed -n 's/^PROJECT_URL ?= //p' Makefile)
python3 -I scripts/site-page.py "$version" "$site_url" "$project_url" >"$site/index.html"
# The page's font, with its licence.
install -D -m 0644 -t "$site/fonts" packaging/site/fonts/martian-mono.woff2 packaging/site/fonts/OFL.txt
install -m 0644 packaging/install.sh "$site/install.sh"
touch "$site/.nojekyll"
echo "Unsigned site for $version is in $site"
