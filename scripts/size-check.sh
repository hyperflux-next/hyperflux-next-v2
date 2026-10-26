#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# size-check.sh [DIR]
#
# Fails when a stripped release binary in DIR (default target/release) is
# 1.5 MiB or more.

set -eu
dir=${1:-target/release}
limit=$((3 * 512 * 1024))
status=0
for binary in hyperflux-next-bridge hyperflux-next-keeper hyperfluxctl; do
    size=$(stat -c %s "$dir/$binary")
    if [ "$size" -lt "$limit" ]; then verdict=ok; else verdict='over 1.5 MiB'; status=1; fi
    printf '%-24s %9d bytes  %s\n' "$binary" "$size" "$verdict"
done
exit "$status"
