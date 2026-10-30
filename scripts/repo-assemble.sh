#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# repo-assemble.sh SITE DIST [PREVIOUS]
#
# Lays out the unsigned package repository in SITE from the packages that
# scripts/package.sh left in DIST/<target>/. PREVIOUS is a checkout of the
# published site. Its current release is carried over so people can go
# back one version. The index files are made with each distribution's own
# tools, in its container. scripts/repo-sign.sh signs the result.

set -euo pipefail
cd "$(dirname "$0")/.."
site=$(realpath -m "${1:?usage: repo-assemble.sh SITE DIST [PREVIOUS]}")
dist=$(realpath "${2:?usage: repo-assemble.sh SITE DIST [PREVIOUS]}")
previous=${3:+$(realpath "$3")}
# shellcheck source=packaging/repository.conf
. packaging/repository.conf
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
site_url=$(sed -n 's/^SITE_URL ?= //p' Makefile)
engine=${CONTAINER_ENGINE:-$(command -v podman || command -v docker)}
in_image() { # target command
    "$engine" build -q -t "hyperflux-next-build:$1" -f "packaging/containers/$1.Containerfile" packaging/containers >/dev/null
    "$engine" run --rm -v "$site:/site" -w /site "hyperflux-next-build:$1" bash -euc "$2"
}
# Copies DIST/TARGET/PATTERN, plus the previous release's matching files.
collect() { # target pattern destination
    mkdir -p "$3"
    # shellcheck disable=SC2086 # the pattern is meant to glob
    cp "$dist/$1"/$2 "$3/"
    if [ -n "$previous" ] && [ -f "$previous/VERSION" ]; then
        local old
        old=$(cat "$previous/VERSION")
        [ "$old" = "$version" ] || find "$previous/${3#"$site"/}" -maxdepth 1 -name "*$old-[0-9]*" -exec cp {} "$3/" \;
    fi
}

rm -rf "$site"
mkdir -p "$site"
echo "$version" >"$site/VERSION"

collect arch '*.pkg.tar.zst' "$site/arch"
in_image arch "cd arch && repo-add -q hyperflux-next.db.tar.zst ./*-$version-*.pkg.tar.zst \
    && rm hyperflux-next.db hyperflux-next.files \
    && cp hyperflux-next.db.tar.zst hyperflux-next.db && cp hyperflux-next.files.tar.zst hyperflux-next.files"

# Release is written next to the suite and moved in: written in place, the
# directory apt-ftparchive hashes would hold half of Release itself.
for target in ubuntu debian; do
    if [ "$target" = ubuntu ]; then suite=$UBUNTU_SUITE name=Ubuntu; else suite=$DEBIAN_SUITE name=Debian; fi
    collect "$target" '*.deb' "$site/$target/pool/main/h/hyperflux-next-v2"
    in_image "$target" "apt-get update -qq && apt-get install -y -qq apt-utils >/dev/null
        cd $target && mkdir -p dists/$suite/main/binary-amd64
        apt-ftparchive --arch amd64 packages pool/main >dists/$suite/main/binary-amd64/Packages
        gzip -9n <dists/$suite/main/binary-amd64/Packages >dists/$suite/main/binary-amd64/Packages.gz
        apt-ftparchive -o APT::FTPArchive::Release::Origin='HyperFlux Next V2' \
            -o APT::FTPArchive::Release::Label='HyperFlux Next V2' -o APT::FTPArchive::Release::Suite=$suite \
            -o APT::FTPArchive::Release::Codename=$suite -o APT::FTPArchive::Release::Architectures=amd64 \
            -o APT::FTPArchive::Release::Components=main \
            -o APT::FTPArchive::Release::Description='HyperFlux Next V2 packages for $name' \
            release dists/$suite >dists/$suite.Release && mv dists/$suite.Release dists/$suite/Release"
    printf '%s\n' 'Types: deb' "URIs: $site_url/$target" "Suites: $suite" 'Components: main' \
        'Architectures: amd64' 'Signed-By: /etc/apt/keyrings/hyperflux-next.asc' >"$site/$target/hyperflux-next.sources"
done

for target in fedora opensuse; do
    if [ "$target" = fedora ]; then
        release=$FEDORA_RELEASE name=Fedora base="$site_url/fedora/\$releasever/\$basearch/" extra=
    else
        release=tumbleweed name=openSUSE base="$site_url/opensuse/tumbleweed/\$basearch/" extra=autorefresh=1
    fi
    collect "$target" '*.rpm' "$site/$target/$release/x86_64/packages"
    in_image "$target" "command -v createrepo_c >/dev/null || dnf install -y -q createrepo_c >/dev/null 2>&1 \
        || zypper --non-interactive -q install createrepo_c >/dev/null
        createrepo_c --quiet --no-database --general-compress-type gz $target/$release/x86_64"
    printf '%s\n' '[hyperflux-next]' "name=HyperFlux Next V2 for $name" "baseurl=$base" 'type=rpm-md' \
        'enabled=1' 'gpgcheck=1' 'repo_gpgcheck=1' "gpgkey=$site_url/hyperflux-next-repository-key.asc" \
        ${extra:+"$extra"} >"$site/$target/hyperflux-next.repo"
done

project_url=$(sed -n 's/^PROJECT_URL ?= //p' Makefile)
cat >"$site/index.html" <<EOF
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>HyperFlux Next V2 package repository</title>
<h1>HyperFlux Next V2 package repository</h1>
<p>HyperFlux Next V2 $version for Arch Linux, Ubuntu $UBUNTU_RELEASE, Debian $DEBIAN_RELEASE,
Fedora $FEDORA_RELEASE and openSUSE Tumbleweed. Follow the install guide for your
distribution; it adds this repository and checks the project signing key.</p>
<p>Or paste one line. It runs the guide's steps for you and asks before it
installs anything:</p>
<pre>curl -fsSL $site_url/install.sh | sh</pre>
<ul>
<li><a href="$project_url/blob/main/docs/user/install-arch.md">Arch Linux</a></li>
<li><a href="$project_url/blob/main/docs/user/install-apt.md">Ubuntu and Debian</a></li>
<li><a href="$project_url/blob/main/docs/user/install-rpm.md">Fedora and openSUSE</a></li>
</ul>
<p>Source, issues and release notes: <a href="$project_url">$project_url</a></p>
</html>
EOF
install -m 0644 packaging/install.sh "$site/install.sh"
touch "$site/.nojekyll"
echo "Unsigned site for $version is in $site"
