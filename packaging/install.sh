#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# install.sh [--core] [--yes]
#
# The install guides' steps in one go, for
#   curl -fsSL https://hyperflux-next.github.io/hyperflux-next-v2/install.sh | sh
# It adds the project's package repository and signing key, installs Full
# (Core with --core) with the kernel headers DKMS needs, enables the bridge
# and your socket, and offers to reboot. The package manager shows what it
# will change and asks first, unless --yes answers for you. Running it
# again does no harm.

SITE=https://hyperflux-next.github.io/hyperflux-next-v2
# The key file the release notes list, pinned by hash: any other file is
# refused. pacman is told to trust its primary key.
KEY_SHA256=e9238836e50b4fea26457ea1a2c29bb38c2fb63202374d4a035d33b616f1cfca
KEY_FPR=206874CADD1B64B32483E2A85FC5921DE97AF7AA

say() { printf '==> %s\n' "$*"; }
die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 3
}
as_root() { if [ -n "$sudo" ]; then sudo "$@"; else "$@"; fi; }
fetch() { curl --proto '=https' --tlsv1.2 -fsSL -o "$2" "$1"; }

# A step that fails stops everything after it. die has said why already.
cleanup() {
    status=$?
    rm -rf "$tmp"
    if [ "$status" != 0 ] && [ "$status" != 3 ]; then
        echo "install.sh: stopped at the step above. The install guide for your distribution has the same steps by hand." >&2
    fi
}

# With curl | sh the script itself is standard input, so the package
# manager's questions are asked on the terminal instead.
pm() {
    if [ -n "$yes" ]; then
        as_root "$@"
    else
        as_root "$@" </dev/tty
    fi
}

install_arch() {
    say "Giving the key to pacman"
    as_root pacman-key --add "$tmp/key.asc" >/dev/null
    as_root pacman-key --lsign-key "$KEY_FPR" >/dev/null
    if ! grep -q '^\[hyperflux-next\]' /etc/pacman.conf; then
        say "Adding the repository to /etc/pacman.conf"
        printf '\n[hyperflux-next]\nSigLevel = Required DatabaseRequired TrustedOnly\nServer = %s/arch\n' "$SITE" |
            as_root tee -a /etc/pacman.conf >/dev/null
    fi
    pkgs=hyperflux-next-v2-linux
    if [ "$edition" = full ]; then
        pkgs=hyperflux-next-v2-full
        for kernel in linux linux-lts linux-zen linux-hardened; do
            pacman -Q "$kernel" >/dev/null 2>&1 && pkgs="$pkgs $kernel-headers"
        done
    fi
    say "Installing $pkgs"
    # --ask 4 lets --yes replace Arch's openrazer-driver-dkms, which pacman
    # would otherwise refuse without a question answered.
    # shellcheck disable=SC2086
    pm pacman -Syu --needed ${yes:+--noconfirm --ask 4} $pkgs
}

install_apt() {
    say "Adding the key and the repository"
    as_root install -d -m 0755 /etc/apt/keyrings
    as_root install -m 0644 "$tmp/key.asc" /etc/apt/keyrings/hyperflux-next.asc
    fetch "$SITE/$target/hyperflux-next.sources" "$tmp/sources"
    as_root install -m 0644 "$tmp/sources" /etc/apt/sources.list.d/hyperflux-next.sources
    as_root apt-get update
    pkgs=hyperflux-next-v2-linux
    if [ "$edition" = full ]; then
        headers=linux-headers-generic
        [ "$target" = debian ] && headers=linux-headers-amd64
        pkgs="hyperflux-next-v2-full $headers"
    fi
    say "Installing $pkgs"
    # shellcheck disable=SC2086
    pm apt-get install ${yes:+-y} $pkgs
}

install_dnf() {
    say "Adding the key and the repository"
    as_root rpm --import "$tmp/key.asc"
    fetch "$SITE/fedora/hyperflux-next.repo" "$tmp/repo"
    as_root install -m 0644 "$tmp/repo" /etc/yum.repos.d/hyperflux-next.repo
    pkgs=hyperflux-next-v2-linux
    erase=
    if [ "$edition" = full ]; then
        pkgs="hyperflux-next-v2-full kernel-devel"
        rpm -q openrazer-kernel-modules-dkms >/dev/null 2>&1 && erase=--allowerasing
    fi
    say "Installing $pkgs"
    # shellcheck disable=SC2086
    pm dnf install ${yes:+-y} $erase $pkgs
}

install_zypper() {
    say "Adding the key and the repository"
    as_root rpm --import "$tmp/key.asc"
    zypper -q lr hyperflux-next >/dev/null 2>&1 ||
        as_root zypper -q addrepo "$SITE/opensuse/hyperflux-next.repo"
    # shellcheck disable=SC2086
    pm zypper ${yes:+--non-interactive} refresh hyperflux-next
    pkgs=hyperflux-next-v2-linux
    resolve=
    if [ "$edition" = full ]; then
        pkgs="hyperflux-next-v2-full kernel-default-devel"
        [ -n "$yes" ] && rpm -q openrazer-kernel-modules-dkms >/dev/null 2>&1 && resolve=--force-resolution
    fi
    say "Installing $pkgs"
    # shellcheck disable=SC2086
    pm zypper ${yes:+--non-interactive} install $resolve $pkgs
}

main() {
    set -eu
    edition=full
    yes=
    for arg; do
        case $arg in
        --core) edition=core ;;
        --yes) yes=1 ;;
        *) die "usage: install.sh [--core] [--yes]" ;;
        esac
    done
    if [ -z "$yes" ] && ! [ -r /dev/tty ]; then
        die "there's no terminal to ask on. Run it with --yes to answer yes to everything."
    fi

    if [ "$(id -u)" = 0 ]; then
        sudo=
        uid=${SUDO_UID:-}
    else
        command -v sudo >/dev/null || die "this needs sudo, or run it as root"
        sudo=sudo
        uid=$(id -u)
    fi

    [ "$(uname -m)" = x86_64 ] || die "the packages are for x86_64 only"
    [ -r /etc/os-release ] || die "can't tell which distribution this is"
    # shellcheck disable=SC1091
    . /etc/os-release
    target=
    case ${ID:-} in
    arch) target=arch ;;
    ubuntu) [ "${VERSION_ID:-}" = 26.10 ] && target=ubuntu ;;
    debian) case ${VERSION_CODENAME:-}:${VERSION_ID:-} in forky:* | *:14) target=debian ;; esac ;;
    fedora) [ "${VERSION_ID:-}" = 44 ] && target=fedora ;;
    opensuse-tumbleweed) target=opensuse ;;
    esac
    [ -n "$target" ] || die "${PRETTY_NAME:-this system} isn't supported. HyperFlux Next V2 has packages for Arch Linux, Ubuntu 26.10, Debian 14, Fedora 44 and openSUSE Tumbleweed."

    tmp=$(mktemp -d)
    trap cleanup EXIT
    say "Getting the project signing key"
    fetch "$SITE/hyperflux-next-repository-key.asc" "$tmp/key.asc" || die "can't download the key from $SITE"
    got=$(sha256sum "$tmp/key.asc" | cut -d' ' -f1)
    [ "$got" = "$KEY_SHA256" ] || die "the key on $SITE isn't the one this script expects, so nothing was changed. Follow the install guide instead."

    case $target in
    arch) install_arch ;;
    ubuntu | debian) install_apt ;;
    fedora) install_dnf ;;
    opensuse) install_zypper ;;
    esac

    say "Enabling the bridge and your socket"
    if [ -n "$uid" ] && [ "$uid" != 0 ]; then
        as_root systemctl enable hyperflux-next-bridge.service "hyperflux-next-client@$uid.socket"
    else
        as_root systemctl enable hyperflux-next-bridge.service
        echo "Each desktop user also needs their socket: systemctl enable hyperflux-next-client@UID.socket"
    fi

    echo
    echo "HyperFlux Next V2 is installed. Reboot to finish, then run: hyperfluxctl doctor"
    if [ -z "$yes" ]; then
        printf 'Reboot now? [y/N] '
        read -r answer </dev/tty || answer=
        case $answer in [yY]*) as_root systemctl reboot ;; esac
    fi
}

main "$@"
