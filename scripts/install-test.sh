#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# install-test.sh TARGET VERSION
#
# Runs as root in a fresh container of TARGET's build image, with the built
# packages in /dist/TARGET and the source tree in /src. Installs Core and
# runs the installed bridge and device keeper, each as its own user,
# against the simulator that package.sh kept from the build (no real
# device is involved). Then installs Full with the distribution's stock
# OpenRazer on top, checks what users rely on, loads the OpenRGB plugin
# into the OpenRGB it's built for, and removes everything again. Core has
# no kernel module. Kernel headers are there for Full's OpenRazer drivers.
# scripts/package.sh starts it.

set -euo pipefail

target=$1 version=$2 pkgs=/dist/$1
openrazer=3.12.4
group=plugdev
stock=(openrazer-daemon python3-openrazer)
units=/usr/lib/systemd/system rules=/usr/lib/udev/rules.d
bridge_bin=/usr/lib/hyperflux-next/hyperflux-next-bridge
keeper_bin=/usr/lib/hyperflux-next/hyperflux-next-keeper
plugin=/usr/lib/openrgb/plugins/hyperflux-next-openrgb.so
full_json=/usr/share/hyperflux-next/components/full.json
# OpenRGB 1.1 from openrgb.org, which the Ubuntu and Fedora plugin packages
# are built for, and the hidapi-hotplug packages it needs: the files the
# install guides name.
openrgb_org=https://codeberg.org/OpenRGB/OpenRGB/releases/download/release_1.1
hidapi_org=https://codeberg.org/OpenRGB/hidapi-hotplug/releases/download/hidapi-hotplug-0.15.0
fail() { echo "install-test: $*" >&2; exit 1; }
pkg() { find "$pkgs" -maxdepth 1 -name "hyperflux-next-v2-$1[-_][0-9]*" ! -name '*.sig' | head -n 1; }

case $target in
arch)
    group=openrazer stock=(openrazer-daemon python-openrazer)
    get() { pacman -S --noconfirm --needed "$@"; }
    add() { pacman -U --noconfirm "$@"; }
    remove() { pacman -Rns --noconfirm "$@"; }
    installed() { pacman -Qq | grep -qx "$1"; } # -Q alone also matches what a package provides
    contents() { pacman -Qlq "$1"; }
    daemon_version() { pacman -Q openrazer-daemon | awk '{print $2}'; }
    pacman -Syu --noconfirm
    get linux-headers linux-lts-headers
    ;;
ubuntu | debian)
    export DEBIAN_FRONTEND=noninteractive
    get() { apt-get install -y --no-install-recommends "$@"; }
    add() { apt-get install -y --no-install-recommends "$@"; }
    remove() { apt-get purge -y "$@"; }
    installed() { dpkg-query -W -f '${Status}' "$1" 2>/dev/null | grep -q '^install ok installed'; }
    contents() { dpkg-query -L "$1"; }
    daemon_version() { dpkg-query -W -f '${Version}' openrazer-daemon; }
    # Maintainer scripts must not start services. This makes any try fail.
    printf '#!/bin/sh\nexit 101\n' >/usr/sbin/policy-rc.d
    chmod 0755 /usr/sbin/policy-rc.d
    apt-get update
    if [ "$target" = ubuntu ]; then
        get linux-headers-generic
        official=("4c12ad429aad50b6e38ff7a04be37268a2401b76884d4067f70ed9df37ebbd2d $hidapi_org/libhidapi-hotplug-hidraw0_0.15.0-1_trixie_amd64.deb"
            "ff4c563d396a80acd876454bf831af3e6578e4bd1d0b5b47990f75e6577450a6 $hidapi_org/libhidapi-hotplug-libusb0_0.15.0-1_trixie_amd64.deb"
            "45d1ecf6a5d7b8e309ec6edd45a5759eac4eff14101ed210a87b88d121747d25 $openrgb_org/openrgb_1.1_amd64_trixie_8e16de2.deb")
    else
        get linux-headers-amd64
    fi
    ;;
fedora | opensuse)
    if [ "$target" = fedora ]; then
        get() { dnf install -y --setopt=install_weak_deps=False "$@"; }
        remove() { dnf remove -y "$@"; }
        repos=/etc/yum.repos.d obs=Fedora_44
        official=("8c63bf116e21781e9bd92735f766b160c334920279b0700326c08ce19311d56c $hidapi_org/hidapi-hotplug-0.15.0-3.fc43.x86_64.rpm"
            "769a625caf492d8dfdf6b68223c8146ac7bb4ec3bcdafe1c54be39a767f62739 $openrgb_org/openrgb_1.1_x86_64_f44_8e16de2.rpm")
        get kernel-devel
    else
        get() { zypper --non-interactive install --no-recommends --allow-unsigned-rpm "$@"; }
        remove() { zypper --non-interactive remove "$@"; }
        repos=/etc/zypp/repos.d obs=openSUSE_Tumbleweed
        get kernel-default-devel
    fi
    add() { get "$@"; }
    installed() { rpm -q "$1" >/dev/null 2>&1; }
    contents() { rpm -ql "$1"; }
    daemon_version() { rpm -q --qf '%{VERSION}' openrazer-daemon; }
    ;;
*) fail "unknown target $target" ;;
esac

# Kernel headers without a kernel: point /lib/modules/*/build at them so
# DKMS builds against every set, like it would on a real system.
kernels=
for dir in /usr/lib/modules/*/build /usr/src/kernels/* /usr/src/linux-*-obj/*/*; do
    [ -f "$dir/include/config/kernel.release" ] || continue
    kernel=$(cat "$dir/include/config/kernel.release")
    case " $kernels " in *" $kernel "*) continue ;; esac
    kernels="$kernels $kernel"
    [ -e "/lib/modules/$kernel/build" ] || { mkdir -p "/lib/modules/$kernel"; ln -s "$dir" "/lib/modules/$kernel/build"; }
done
[ -n "$kernels" ] || fail "no kernel headers installed"

built() { # module version: build it for every kernel and check the result
    for kernel in $kernels; do
        dkms build --force -m "$1" -v "$2" -k "$kernel" >&2
        modules=$(find "/var/lib/dkms/$1/$2/$kernel" -name '*.ko*')
        [ -n "$modules" ] || fail "$1 wasn't built for $kernel"
        for ko in $modules; do
            [ "$(modinfo -F vermagic "$ko" | cut -d' ' -f1)" = "$kernel" ] || fail "$ko has the wrong vermagic"
        done
        echo "$modules"
    done
}

no_enabled_units() {
    if find /etc/systemd -name '*hyperflux*' | grep -q .; then fail "installing enabled a unit"; fi
}

within() { # seconds command...: whether the command succeeds in time
    local deadline=$((SECONDS + $1))
    shift
    until "$@"; do
        [ "$SECONDS" -lt "$deadline" ] || return 1
        sleep 0.2
    done
}

# Like the unit: user hyperflux-next, group hyperflux, and the supplementary
# group hyperflux-next. An array, not a function: a function in the
# background is a subshell, and $! would be its pid instead of the program's.
as_bridge=(setpriv --reuid=hyperflux-next --regid=hyperflux --init-groups)
as_keeper=(setpriv --reuid=hyperflux-keeper --regid=hyperflux --init-groups)

# The installed bridge and keeper on the simulator, in the runtime directory
# systemd would make for the bridge. hyperfluxctl reaches the bridge through
# the system socket, and each daemon checks the other's user on theirs.
bridge_on_simulator() {
    local sim=$pkgs/test/hyperflux-next-sim work
    [ -x "$sim" ] || fail "$sim is missing; package.sh keeps it from the build"
    work=$(mktemp -d)
    chown hyperflux-next "$work"
    chmod 0755 "$work"
    install -d -m 2750 -o hyperflux-next -g hyperflux /run/hyperflux-next
    "${as_bridge[@]}" "$sim" --socket "$work/sim.sock" </dev/null >"$work/sim.log" 2>&1 &
    local sim_pid=$!
    within 10 test -S "$work/sim.sock" || { cat "$work/sim.log" >&2; fail "the simulator didn't start"; }
    # The keepers below read it too.
    chmod 0666 "$work/sim.sock"
    "${as_bridge[@]}" "$bridge_bin" --sim "$work/sim.sock" 2>"$work/bridge.log" &
    local bridge_pid=$!
    within 10 test -S /run/hyperflux-next/bridge.sock || { cat "$work/bridge.log" >&2; fail "the bridge didn't start"; }
    # Another member of the hyperflux group reaches the keeper's socket, and
    # is turned away for its uid.
    setpriv --reuid=65534 --regid=hyperflux --clear-groups "$keeper_bin" --sim "$work/sim.sock" 2>"$work/other.log" &
    local other_pid=$!
    within 10 grep -q 'refused a device keeper: it runs as uid 65534' "$work/bridge.log" ||
        { cat "$work/bridge.log" "$work/other.log" >&2; fail "the bridge took a keeper of another user"; }
    kill "$other_pid"
    wait "$other_pid" 2>/dev/null || true
    "${as_keeper[@]}" "$keeper_bin" --sim "$work/sim.sock" 2>"$work/keeper.log" &
    local keeper_pid=$!
    within 10 grep -q 'connected to the bridge' "$work/keeper.log" ||
        { cat "$work/bridge.log" "$work/keeper.log" >&2; fail "the keeper didn't reach the bridge"; }
    # The bridge probes both batteries as soon as it admits the receiver.
    expected=$'Razer Basilisk V3 Pro 35K: 79% charging\nRazer DeathStalker V2 Pro TKL: 64%'
    batteries() { [ "$(hyperfluxctl battery 2>&1)" = "$expected" ]; }
    within 10 batteries || { hyperfluxctl battery >&2; cat "$work/bridge.log" >&2; fail "the batteries didn't arrive"; }
    echo "The installed bridge on the simulator:"
    hyperfluxctl battery
    hyperfluxctl devices --json | grep -F '"device_id": "keyboard-0296"' >/dev/null || fail "hyperfluxctl devices doesn't list the keyboard"
    # The keeper made the battery devices the bridge asked for.
    within 10 grep -q 'simulated battery Razer Basilisk V3 Pro 35K: 79% charging' "$work/keeper.log" ||
        { cat "$work/keeper.log" >&2; fail "the keeper didn't show the mouse's battery"; }
    # The hyperflux group may use the system socket, everyone else may not.
    setpriv --reuid=65534 --regid=hyperflux --clear-groups hyperfluxctl battery >/dev/null ||
        fail "a member of hyperflux can't reach the bridge"
    if setpriv --reuid=65534 --regid=65534 --clear-groups hyperfluxctl battery >/dev/null 2>&1; then
        fail "a user outside hyperflux reached the bridge"
    fi
    kill -TERM "$keeper_pid"
    wait "$keeper_pid" || { cat "$work/keeper.log" >&2; fail "the keeper didn't stop cleanly"; }
    kill -TERM "$bridge_pid"
    wait "$bridge_pid" || { cat "$work/bridge.log" >&2; fail "the bridge didn't stop cleanly"; }
    for socket in bridge keeper; do
        [ ! -e "/run/hyperflux-next/$socket.sock" ] || fail "the bridge left $socket.sock behind"
    done
    kill "$sim_pid"
    wait "$sim_pid" 2>/dev/null || true
    rm -rf "$work" /run/hyperflux-next
}

# The openrgb.org packages, checked against the checksums above.
official_openrgb() {
    local dir entry files=() stub
    dir=$(mktemp -d) stub=$(mktemp -d)
    for entry in "${official[@]}"; do
        curl -fsSL --retry 3 -o "$dir/${entry##*/}" "${entry#* }"
        echo "${entry%% *}  $dir/${entry##*/}" | sha256sum -c --quiet || fail "${entry##*/} has changed"
        files+=("$dir/${entry##*/}")
    done
    # Their postinst runs "udevadm trigger", which would replay every
    # device of the machine running this if the container could write /sys.
    printf '#!/bin/sh\nexit 0\n' >"$stub/udevadm"
    chmod 0755 "$stub/udevadm"
    PATH=$stub:$PATH add "${files[@]}"
}

# OpenRGB without a display and with an empty configuration has to find
# the plugin in its system plugin directory.
openrgb_loads_plugin() {
    local work pid tries=0
    work=$(mktemp -d)
    mkdir "$work/config"
    mkdir -m 0700 "$work/runtime"
    QT_QPA_PLATFORM=offscreen XDG_RUNTIME_DIR=$work/runtime HYPERFLUX_NEXT_SOCKET=$work/no-bridge.sock \
        openrgb --config "$work/config" --nodetect --noautoconnect --verbose >"$work/log" 2>&1 &
    pid=$!
    until grep -q 'Loaded plugin HyperFlux Next' "$work/log"; do
        if ! kill -0 "$pid" 2>/dev/null || [ "$tries" -ge 60 ]; then
            cat "$work/log" >&2
            fail "OpenRGB didn't load $plugin"
        fi
        tries=$((tries + 1))
        sleep 0.5
    done
    # It has to survive building its page and failing to connect.
    sleep 2
    kill -0 "$pid" 2>/dev/null || { cat "$work/log" >&2; fail "OpenRGB stopped after loading $plugin"; }
    grep 'Loaded plugin HyperFlux Next' "$work/log"
    kill "$pid"
    wait "$pid" 2>/dev/null || true
}

echo "== Core"
add "$(pkg linux)"
no_enabled_units
if command -v dkms >/dev/null && dkms status | grep -q .; then fail "Core registered a DKMS module"; fi
getent group hyperflux >/dev/null || fail "no hyperflux group"
for user in hyperflux-next hyperflux-keeper; do
    getent passwd "$user" >/dev/null || fail "no $user user"
    id -nG "$user" | tr ' ' '\n' | grep -qx hyperflux || fail "$user isn't in the hyperflux group"
done
for file in /usr/bin/hyperfluxctl "$bridge_bin" "$keeper_bin" "$rules/99-hyperflux-next.rules" \
    /usr/lib/modules-load.d/hyperflux-next.conf /usr/lib/sysusers.d/hyperflux-next.conf \
    "$units/hyperflux-next-bridge.service" "$units/hyperflux-next-keeper.service" \
    "$units/hyperflux-next-client@.socket" \
    "$units/hyperflux-next-client@.service" /usr/share/bash-completion/completions/hyperfluxctl \
    /usr/share/zsh/site-functions/_hyperfluxctl /usr/share/fish/vendor_completions.d/hyperfluxctl.fish; do
    [ -e "$file" ] || fail "$file is missing"
done
# From the package's file list: container images often skip man pages.
contents hyperflux-next-v2-linux | grep '^/usr/share/man/man1/hyperfluxctl\.1' >/dev/null || fail "the man page is missing"
hyperfluxctl --version | grep -qF "$version" || fail "hyperfluxctl doesn't report $version"
[ "$("$bridge_bin" --version)" = "$version" ] || fail "the bridge doesn't report $version"
[ "$("$keeper_bin" --version)" = "$version" ] || fail "the keeper doesn't report $version"
# Names are resolved: this also fails when the group is missing.
udevadm verify "$rules/99-hyperflux-next.rules" >/dev/null || fail "udevadm rejects 99-hyperflux-next.rules"
scratch=$(mktemp -d)
for unit in socket service; do
    cp "$units/hyperflux-next-client@.$unit" "$scratch/hyperflux-next-client@1000.$unit"
done
systemd-analyze verify --man=no "$units/hyperflux-next-bridge.service" \
    "$units/hyperflux-next-keeper.service" "$scratch"/* ||
    fail "systemd-analyze rejects the units"
rm -rf "$scratch"
# What the install guides tell people to enable.
systemctl enable hyperflux-next-bridge.service hyperflux-next-client@1000.socket
for link in multi-user.target.wants/hyperflux-next-bridge.service \
    user-runtime-dir@1000.service.wants/hyperflux-next-client@1000.socket; do
    [ -L "/etc/systemd/system/$link" ] || fail "enabling didn't create $link"
done
systemctl disable hyperflux-next-bridge.service hyperflux-next-client@1000.socket
no_enabled_units
bridge_on_simulator

echo "== Full with the stock OpenRazer"
case $target in
# Full depends on the plugin package where the distribution's OpenRGB loads it.
arch | debian) add "$(pkg openrazer-driver-dkms)" "$(pkg full)" "$(pkg openrgb-plugin)" ;;
*) add "$(pkg openrazer-driver-dkms)" "$(pkg full)" ;;
esac
case $target in
arch | ubuntu | debian) get "${stock[@]}" ;;
*)
    # OpenRazer for Fedora and openSUSE comes from its own OBS project.
    # Trust its key only if it's the one this test knows.
    url=https://download.opensuse.org/repositories/hardware:/razer/$obs
    curl -fsSL "$url/repodata/repomd.xml.key" -o /tmp/obs.key
    fpr=$(gpg --show-keys --with-colons /tmp/obs.key | awk -F: '$1 == "fpr" {print $10; exit}')
    [ "$fpr" = E96C496E4E77C159C2FCA053062F9FD4D6D11CE4 ] || fail "unexpected OBS key $fpr"
    rpm --import /tmp/obs.key
    curl -fsSL "$url/hardware:razer.repo" -o "$repos/hardware_razer.repo"
    get "${stock[@]}"
    ;;
esac
no_enabled_units
for name in openrazer-driver-dkms openrazer-kernel-modules-dkms; do
    if installed "$name"; then fail "the distribution's $name was installed next to Full"; fi
done
daemon=$(daemon_version)
case $daemon in
"$openrazer"[-+~]* | "$openrazer") ;;
*) fail "openrazer-daemon is $daemon, but the packaged driver is $openrazer: rebuild against it" ;;
esac
getent group "$group" >/dev/null || fail "no $group group"
grep -qF "\"version\":\"$version\"" "$full_json" || fail "full.json names another version"
for file in 69-hyperflux-next-openrazer-access.rules 70-hyperflux-next-openrazer-keys.rules 99-razer.rules; do
    udevadm verify "$rules/$file" >/dev/null || fail "udevadm rejects $file"
done
# An assignment: a failed build stops the test here.
modules=$(built openrazer-driver "$openrazer")
echo "openrazer-driver $openrazer built for$kernels"
for ko in $modules; do
    case $ko in *razerkbd* | *razermouse*)
        [ "$(modinfo -F hyperflux_native_transport "$ko")" = razer-feature-report-v1 ] ||
            fail "$ko doesn't advertise the HyperFlux transport" ;;
    esac
done

echo "== OpenRGB plugin"
ours=(hyperflux-next-v2-full hyperflux-next-v2-openrazer-driver-dkms hyperflux-next-v2-linux)
case $target in
arch | debian)
    [ -f "$plugin" ] || fail "Full didn't bring $plugin"
    get openrgb
    stock+=(openrgb)
    ;;
ubuntu | fedora)
    [ ! -e "$plugin" ] || fail "Full installed $plugin without OpenRGB 1.1"
    if add "$(pkg openrgb-plugin)" >/dev/null 2>&1; then
        fail "the plugin package installed next to the distribution's own OpenRGB"
    fi
    official_openrgb
    add "$(pkg openrgb-plugin)"
    if [ "$target" = ubuntu ]; then
        stock+=(openrgb libhidapi-hotplug-hidraw0 libhidapi-hotplug-libusb0)
    else
        stock+=(openrgb hidapi-hotplug)
    fi
    ;;
opensuse)
    [ -z "$(pkg openrgb-plugin)" ] || fail "an openrgb-plugin package was built for openSUSE, which has no OpenRGB for it"
    [ ! -e "$plugin" ] || fail "$plugin is installed"
    ;;
esac
if [ -e "$plugin" ]; then
    ours=(hyperflux-next-v2-openrgb-plugin "${ours[@]}")
    openrgb_loads_plugin
fi

echo "== Removal"
remove "${stock[@]}" "${ours[@]}"
# pacman -Rns may take dkms itself away. Look at what DKMS leaves on disk.
for path in /usr/lib/hyperflux-next /usr/bin/hyperfluxctl "$rules/99-hyperflux-next.rules" \
    "$units/hyperflux-next-bridge.service" "$units/hyperflux-next-keeper.service" "$full_json" "$plugin" \
    "/usr/src/openrazer-driver-$openrazer" /var/lib/dkms/openrazer-driver; do
    [ ! -e "$path" ] || fail "$path is still there"
done
# The docs say the system users stay, as distributions keep them.
for user in hyperflux-next hyperflux-keeper; do
    getent passwd "$user" >/dev/null || fail "removal deleted the $user user"
done
echo "install-test: $target passed"
