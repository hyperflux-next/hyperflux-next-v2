#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# headless-load.sh PLUGIN [OPENRGB]
#
# Starts OpenRGB without a display, with a throwaway configuration holding
# only PLUGIN, and waits for the "Loaded plugin HyperFlux Next V2" line.
# OpenRGB is told not to detect hardware, and the plugin is pointed at a
# bridge socket that doesn't exist. It gets as far as building its page and
# trying to connect. Use an OpenRGB of the release the plugin was built for.
#
# An installed copy in OpenRGB's system plugin directory would load in place
# of a broken build, so that directory is covered with an empty one (bwrap,
# else unshare). If neither works it has to be empty.

set -eu

plugin=$(realpath "${1:?usage: headless-load.sh PLUGIN [OPENRGB]}")
openrgb=$(command -v "${2:-openrgb}")

work=$(mktemp -d "${TMPDIR:-/tmp}/openrgb-load.XXXXXX")
pid=
cleanup() {
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$work/config/plugins" "$work/runtime" "$work/empty"
chmod 0700 "$work/runtime"
cp "$plugin" "$work/config/plugins/"

# The system directory is compiled into OpenRGB, and its path is in the binary.
system=$(grep -a -o -m 1 '/[[:alnum:]/_.-]*/openrgb/plugins' "$openrgb" | head -n 1 || true)
run="env"
if [ -n "$system" ] && [ -d "$system" ]; then
    if command -v bwrap >/dev/null 2>&1 && bwrap --dev-bind / / --tmpfs "$system" true 2>/dev/null; then
        run="bwrap --dev-bind / / --tmpfs $system --die-with-parent env"
    elif unshare --user --map-root-user --mount true 2>/dev/null; then
        run="unshare --user --map-root-user --mount sh -c 'mount --bind $work/empty $system && exec env \"\$@\"' sh"
    elif [ -n "$(ls -A "$system")" ]; then
        echo "headless-load.sh: can't hide $system, and it holds plugins that could load instead of $plugin" >&2
        exit 1
    fi
fi

eval exec "$run" QT_QPA_PLATFORM=offscreen XDG_RUNTIME_DIR="'$work/runtime'" HYPERFLUX_NEXT_SOCKET="'$work/no-bridge.sock'" \
    "'$openrgb'" --config "'$work/config'" --nodetect --noautoconnect --verbose >"$work/log" 2>&1 &
pid=$!

tries=0
while [ "$tries" -lt 60 ]; do
    if grep -q "Loaded plugin HyperFlux Next V2" "$work/log"; then
        # Give the plugin a moment to run before OpenRGB is stopped.
        sleep 2
        if kill -0 "$pid" 2>/dev/null; then
            grep "Loaded plugin HyperFlux Next V2" "$work/log"
            exit 0
        fi
        break
    fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
    tries=$((tries + 1))
done

cat "$work/log" >&2
echo "headless-load.sh: OpenRGB didn't load $plugin, or stopped after loading it" >&2
exit 1
