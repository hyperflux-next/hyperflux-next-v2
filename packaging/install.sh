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
MOVING=https://github.com/hyperflux-next/hyperflux-next-v2/blob/main/docs/user/moving-from-2x.md
# The releases the site has packages for, as in packaging/repository.conf.
UBUNTU_SUITE=stonking
UBUNTU_RELEASE=26.10
DEBIAN_SUITE=forky
DEBIAN_RELEASE=14
FEDORA_RELEASE=44
SUPPORTED="Arch Linux, Ubuntu $UBUNTU_RELEASE, Debian $DEBIAN_RELEASE, Fedora $FEDORA_RELEASE and openSUSE Tumbleweed"

say() { printf '==> %s\n' "$*"; }
die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 3
}
as_root() { if [ -n "$sudo" ]; then sudo "$@"; else "$@"; fi; }
fetch() { curl --proto '=https' --tlsv1.2 -fsSL -o "$2" "$1"; }
# Shows a command's output only when it fails. pacman-key passes on gpg's
# trust database notes, which read like errors and aren't.
quiet() {
    out=$("$@" 2>&1) || {
        printf '%s\n' "$out" >&2
        return 1
    }
}

# A step that fails stops everything after it. die has said why already.
cleanup() {
    status=$?
    rm -rf "$tmp"
    if [ "$status" != 0 ] && [ "$status" != 3 ]; then
        echo "install.sh: stopped at the step above. The install guide for your distribution, or the one it's based on, has the same steps by hand." >&2
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
    # 2.x used the same repository name, so an existing entry may still
    # point at its server. pacman-conf reads entries pulled in with Include
    # as well, which a grep of pacman.conf would miss.
    server=$(pacman-conf --repo hyperflux-next Server 2>/dev/null || true)
    case $server in
    "" | "$SITE/arch" | "$SITE/arch/") ;;
    *) die "pacman still uses the 2.x repository at $server. Remove that entry as $MOVING shows, then run this again." ;;
    esac
    say "Giving the key to pacman"
    quiet as_root pacman-key --add "$tmp/key.asc"
    quiet as_root pacman-key --lsign-key "$KEY_FPR"
    if [ -z "$server" ]; then
        say "Adding the repository to /etc/pacman.conf"
        printf '\n[hyperflux-next]\nSigLevel = Required DatabaseRequired TrustedOnly\nServer = %s/arch\n' "$SITE" |
            as_root tee -a /etc/pacman.conf >/dev/null
    fi
    # pacman keeps a repository's package list after its entry is removed,
    # and one left by 2.x is signed with the old key. Fetch ours fresh.
    sync=$(pacman-conf DBPath)sync
    as_root rm -f "$sync/hyperflux-next.db" "$sync/hyperflux-next.db.sig" \
        "$sync/hyperflux-next.files" "$sync/hyperflux-next.files.sig"
    pkgs=hyperflux-next-v2-linux
    if [ "$edition" = full ]; then
        pkgs=hyperflux-next-v2-full
        # Each Arch kernel package puts its name in /usr/lib/modules/*/pkgbase,
        # so the loop also catches linux-cachyos or Manjaro's linux612. A
        # kernel whose headers no repository has, such as one built from the
        # AUR, is skipped.
        for f in /usr/lib/modules/*/pkgbase; do
            [ -r "$f" ] || continue
            kernel=$(cat "$f")
            case " $pkgs " in *" $kernel-headers "*) continue ;; esac
            pacman -Si "$kernel-headers" >/dev/null 2>&1 && pkgs="$pkgs $kernel-headers"
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
    uri=$(zypper -q lr hyperflux-next 2>/dev/null | sed -n 's/^URI *: *//p')
    case $uri in
    "" | "$SITE"/*) ;;
    *) die "zypper still uses the 2.x repository at $uri. Remove it as $MOVING shows, then run this again." ;;
    esac
    say "Adding the key and the repository"
    as_root rpm --import "$tmp/key.asc"
    [ -n "$uri" ] || as_root zypper -q addrepo "$SITE/opensuse/hyperflux-next.repo"
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

# Sets target to the site's directory for this system. A derivative lists
# the distribution it's based on in ID_LIKE. The release then comes from
# UBUNTU_CODENAME on Ubuntu's derivatives, DEBIAN_CODENAME or
# /etc/debian_version on Debian's, and VERSION_ID on Fedora's, which use
# Fedora's numbers.
detect() {
    if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        . /etc/os-release
    elif [ -r /usr/lib/os-release ]; then
        # shellcheck disable=SC1091
        . /usr/lib/os-release
    else
        die "can't tell which distribution this is"
    fi
    name=${PRETTY_NAME:-${NAME:-this system}}
    target=
    case " ${ID:-} ${ID_LIKE:-} " in
    *" arch "*) target=arch ;;
    *" ubuntu "*)
        base=${UBUNTU_CODENAME:-${VERSION_CODENAME:-}}
        if [ "$base" = "$UBUNTU_SUITE" ]; then
            target=ubuntu
        elif [ "${ID:-}" != ubuntu ] && [ -n "$base" ]; then
            die "$name is based on Ubuntu $base, and the packages are for Ubuntu $UBUNTU_RELEASE ($UBUNTU_SUITE)."
        fi
        ;;
    *" debian "*)
        if [ "${ID:-}" = debian ]; then
            base=${VERSION_CODENAME:-}:${VERSION_ID:-}
        else
            base=${DEBIAN_CODENAME:-}:$(cat /etc/debian_version 2>/dev/null || true)
        fi
        case $base in
        "$DEBIAN_SUITE":* | *:"$DEBIAN_RELEASE" | *:"$DEBIAN_RELEASE".* | *:"$DEBIAN_SUITE"/*) target=debian ;;
        esac
        ;;
    *" fedora "*) [ "${VERSION_ID:-}" = "$FEDORA_RELEASE" ] && target=fedora ;;
    *" opensuse-tumbleweed "*) target=opensuse ;;
    esac
    [ -n "$target" ] || die "$name isn't supported. HyperFlux Next V2 has packages for $SUPPORTED, and distributions based on those releases can use them."
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
    detect
    # Artix and Devuan boot another init, and the bridge is a systemd
    # service. Stop only when PID 1 is a known other init: a minimal system
    # without systemd yet gets it with the packages.
    if ! [ -d /run/systemd/system ]; then
        case $(cat /proc/1/comm 2>/dev/null) in
        init | runit | s6-svscan | openrc-init | dinit) die "$name doesn't run systemd, and the bridge needs it." ;;
        esac
    fi
    # The last mount listed for / or /usr is the one in effect.
    case $(awk '$2 == "/" || $2 == "/usr" {o = $4} END {print o}' /proc/mounts) in
    ro*) die "$name keeps /usr read-only, so the packages can't be installed on it." ;;
    esac

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
