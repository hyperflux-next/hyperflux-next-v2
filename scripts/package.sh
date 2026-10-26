#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# package.sh dist
# package.sh TARGET [--install-test]
#
# Builds the packages for TARGET (arch, ubuntu, debian, fedora or
# opensuse) with the distribution's own recipe, inside its container, from
# a tarball of the working tree. Lints them with namcap, lintian or rpmlint
# and leaves them in dist/TARGET/, with the simulator the install test
# needs in dist/TARGET/test/. --install-test then runs
# scripts/install-test.sh in a fresh container of the same image.
#
# CONTAINER_ENGINE picks podman or docker (default: whichever exists).
# HFX_LATEST_IMAGES=1 builds on the newest image instead of the pinned one.

set -euo pipefail

inside() {
    local target=$1 version=$2 out=/dist/$1 src=hyperflux-next-v2-$2
    local tarball=/dist/hyperflux-next-v2-$version.tar.gz
    mkdir -p /build "$out"
    cd /build
    case $target in
    arch)
        tar -xzf "$tarball" -O "$src/packaging/arch/PKGBUILD" >PKGBUILD
        cp "$tarball" .
        # shellcheck disable=SC1091,SC2154
        mapfile -t deps < <(. ./PKGBUILD; printf '%s\n' "${makedepends[@]}" "${checkdepends[@]}")
        pacman -Syu --noconfirm --needed "${deps[@]}"
        chown -R builder /build
        su builder -c 'makepkg --noconfirm'
        namcap ./*.pkg.tar.zst | tee "$out/namcap.txt"
        cp ./*.pkg.tar.zst "$out/"
        ;;
    ubuntu | debian)
        tar -xzf "$tarball"
        cp -r "$src/packaging/debian" "$src/debian"
        apt-get update
        # No -dbgsym packages: the Rust binaries ship stripped.
        (cd "$src" && DEBIAN_FRONTEND=noninteractive apt-get build-dep -y ./ &&
            DEB_BUILD_OPTIONS=noautodbgsym dpkg-buildpackage -b -us -uc)
        # These packages never go to the Debian archive. There's no ITP bug
        # for the first changelog entry to close.
        lintian --fail-on error --allow-root --display-level '>=warning' \
            --suppress-tags initial-upload-closes-no-bugs ./*.deb | tee "$out/lintian.txt"
        cp ./*.deb "$out/"
        ;;
    fedora | opensuse)
        local spec=/build/hyperflux-next-v2.spec
        mkdir -p rpm/SOURCES
        cp "$tarball" rpm/SOURCES/
        tar -xzf "$tarball" -O "$src/packaging/rpm/hyperflux-next-v2.spec" >"$spec"
        tar -xzf "$tarball" -O "$src/packaging/rpm/$target.rpmlintrc" >/build/rpmlintrc
        # The other sources are the pinned OpenRazer and (Fedora only)
        # OpenRGB archives. The fetch scripts check their checksums in %build.
        rpmspec -P "$spec" | sed -n 's/^Source[1-9][0-9]*: *//p' | while read -r url; do
            curl -fsSL --retry 3 -o "rpm/SOURCES/${url##*#/}" "${url%#/*}"
        done
        if [ "$target" = fedora ]; then
            dnf builddep -y --setopt=install_weak_deps=False "$spec"
        else
            # zypper wants "cargo>=1.95", without the spaces.
            mapfile -t deps < <(rpmspec -q --buildrequires "$spec" | tr -d ' ')
            zypper --non-interactive install --no-recommends "${deps[@]}"
        fi
        # --noclean keeps the build tree, and the simulator in it, which
        # rpm 4.20 and later remove after a successful build.
        rpmbuild -bb --noclean --define "_topdir /build/rpm" "$spec"
        find rpm/RPMS -name '*.rpm' -exec cp {} "$out/" \;
        # rpmlint's exit status differs between versions. The summary doesn't.
        rpmlint -r /build/rpmlintrc "$out"/*.rpm | tee "$out/rpmlint.txt" || true
        grep -q ' 0 errors' "$out/rpmlint.txt"
        ;;
    esac
    # The install test runs the packaged bridge against the simulator,
    # which no package ships. Keep the one this build made.
    sim=$(find /build -path '*/target/release/hyperflux-next-sim' -type f -print -quit)
    [ -n "$sim" ] || { echo "package.sh: the build left no hyperflux-next-sim" >&2; exit 1; }
    install -Dm0755 "$sim" "$out/test/hyperflux-next-sim"
}

if [ "${1:-}" = --inside ]; then
    inside "$2" "$3"
    exit
fi

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
tarball=dist/hyperflux-next-v2-$version.tar.gz
target=${1:?usage: package.sh dist | package.sh TARGET [--install-test]}

# Tracked and new files, as GitHub's archive of the tag would have them.
mkdir -p dist
git ls-files -z --cached --others --exclude-standard |
    while IFS= read -r -d '' file; do [ -e "$file" ] && printf '%s\0' "$file"; done |
    tar --null -T - --sort=name --owner=0 --group=0 --numeric-owner \
        --mtime="@$(git log -1 --format=%ct 2>/dev/null || echo 0)" \
        --transform "s,^,hyperflux-next-v2-$version/," -czf "$tarball.$$"
# Renamed into place: a build of another target that is reading it never
# sees a half-written file.
mv "$tarball.$$" "$tarball"
[ "$target" = dist ] && { echo "$tarball"; exit; }

containerfile=packaging/containers/$target.Containerfile
[ -f "$containerfile" ] || { echo "package.sh: unknown target $target" >&2; exit 2; }
engine=${CONTAINER_ENGINE:-$(command -v podman || command -v docker)}
image=hyperflux-next-build:$target
if [ "${HFX_LATEST_IMAGES:-}" = 1 ]; then
    sed 's/@sha256:[0-9a-f]*//' "$containerfile" | "$engine" build -t "$image" -f - packaging/containers
else
    "$engine" build -t "$image" -f "$containerfile" packaging/containers
fi

rm -rf "dist/$target"
run() { "$engine" run --rm -v "$PWD:/src:ro" -v "$PWD/dist:/dist" "$image" "$@"; }
run bash /src/scripts/package.sh --inside "$target" "$version"
if [ "${2:-}" = --install-test ]; then
    run bash /src/scripts/install-test.sh "$target" "$version"
fi
# Docker leaves root-owned files behind, but podman maps them to this user.
[ -O "dist/$target" ] || run chown -R "$(id -u):$(id -g)" "/dist/$target"
ls -l "dist/$target"
