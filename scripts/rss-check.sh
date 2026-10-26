#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# rss-check.sh
#
# Starts the bridge and the device keeper against the simulator, lets a few
# clients use the bridge and fails when either's resident memory reaches
# 10 MiB.

set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/sim-stack.sh

for command in status devices battery doctor; do
    HYPERFLUX_NEXT_SOCKET=$socket "$bin/hyperfluxctl" "$command" >/dev/null || true
done
sleep 2
status=0
for name in bridge keeper; do
    rss=$(awk '/^VmRSS:/ {print $2}' "/proc/${!name}/status")
    echo "$name RSS with one simulated receiver: $rss kB (limit 10240 kB)"
    [ "$rss" -lt 10240 ] || status=1
done
exit "$status"
