#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# idle-check.sh [SECONDS]
#
# Counts how often the bridge and the device keeper wake up while the
# simulated receiver is idle, over SECONDS (default 60). Every wake-up ends
# in a context switch. At rest each may wake for its watchdog pings, and
# the bridge, once a minute, to ask the simulated mouse whether it's
# charging (07/80, then 07/84). The simulator counts the queries. Each costs
# the event loop and the exchange thread about 6 switches. A polling loop
# shows up as hundreds.

set -euo pipefail
cd "$(dirname "$0")/.."
seconds=${1:-60}
. scripts/sim-stack.sh

switches() { cat /proc/{"$bridge","$keeper"}/task/*/status | awk '/ctxt_switches/ {n += $2} END {print n}'; }
pings() { grep -c WATCHDOG=1 "$tmp/notify.log" || true; }
queries() {
    echo list >&3
    sleep 0.5
    # The last list's two device lines.
    awk '/ requests$/ {n[++i % 2] = $(NF - 1)} END {print n[0] + n[1]}' "$tmp/sim.log"
}

sleep 5
queries_before=$(queries)
switches_before=$(switches) pings_before=$(pings)
sleep "$seconds"
woke=$(($(switches) - switches_before)) pinged=$(($(pings) - pings_before))
asked=$(($(queries) - queries_before))
# The mouse is asked a minute after its start-up probe, then every minute.
checks=$(((seconds + 6) / 60))
allowed=$((2 * pinged + 8 * asked + 10))
echo "bridge and keeper woke $woke times in $seconds s, with $pinged watchdog pings and $asked queries (allowed $allowed)"
[ "$pinged" -gt 0 ] || { echo "no watchdog pings: is WATCHDOG_USEC honoured?" >&2; exit 1; }
[ "$asked" -le $((2 * checks)) ] || { echo "more than one charge check a minute" >&2; exit 1; }
[ "$woke" -le "$allowed" ]
