# shellcheck shell=bash disable=SC2034
# SPDX-License-Identifier: GPL-2.0-only
#
# Sourced by idle-check.sh and rss-check.sh: starts the simulator, and a
# bridge and a device keeper talking to it, with a stand-in for systemd's
# notify socket so their READY=1 and watchdog pings can be seen. Sets
# $bridge and $keeper (their pids), $tmp and $socket. BIN_DIR picks the
# binaries (default target/release).

bin=${BIN_DIR:-target/release}
tmp=$(mktemp -d)
socket=$tmp/bridge.sock
trap 'kill $(jobs -p) 2>/dev/null; wait 2>/dev/null; rm -rf "$tmp"' EXIT

wait_for() { # seconds command...
    local deadline=$((SECONDS + $1)); shift
    until "$@"; do
        [ "$SECONDS" -lt "$deadline" ] || { echo "timed out waiting for: $*" >&2; cat "$tmp"/*.log >&2; exit 1; }
        sleep 0.2
    done
}

# The simulator reads commands from stdin; keep it open and quiet.
mkfifo "$tmp/sim-input"
exec 3<>"$tmp/sim-input"
"$bin/hyperflux-next-sim" --socket "$tmp/sim.sock" <&3 >"$tmp/sim.log" 2>&1 &
wait_for 10 test -S "$tmp/sim.sock"

python3 -c '
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
s.bind(sys.argv[1])
while True:
    print(s.recv(4096).decode(errors="replace").replace("\n", " "), flush=True)
' "$tmp/notify" >"$tmp/notify.log" &
wait_for 5 test -S "$tmp/notify"

# A 10 s watchdog makes each ping every 5 s, like WatchdogSec=30s does
# every 15 s.
NOTIFY_SOCKET=$tmp/notify WATCHDOG_USEC=10000000 \
    "$bin/hyperflux-next-bridge" --sim "$tmp/sim.sock" --socket "$socket" 2>"$tmp/bridge.log" &
bridge=$!
wait_for 10 grep -q READY=1 "$tmp/notify.log"
NOTIFY_SOCKET=$tmp/notify WATCHDOG_USEC=10000000 \
    "$bin/hyperflux-next-keeper" --sim "$tmp/sim.sock" --socket "$tmp/keeper.sock" 2>"$tmp/keeper.log" &
keeper=$!
wait_for 10 grep -q 'connected to the bridge' "$tmp/keeper.log"
