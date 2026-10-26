#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# loc-check.sh
#
# Counts non-blank lines of the project's own code per part and fails when
# a part or the whole goes over the budget in ARCHITECTURE.md ("Performance
# and size"). Patches, fixtures, vendored files and Markdown don't count.

set -euo pipefail
cd "$(dirname "$0")/.."

# ARCHITECTURE.md, section 6, lists the same budgets.
# Raise one only with the change that needs it, after trimming what can go.
declare -A budget=(
    [hfx-proto]=5000 [hfx-bridge]=16000 [hyperfluxctl]=9000
    [openrgb-plugin+cpp-sdk]=6000 [python-sdk]=600 [scripts+packaging]=2000
)
total_budget=40000
declare -A lines

part() {
    case $1 in
    crates/hfx-proto/*) echo hfx-proto ;;
    crates/hfx-bridge/*) echo hfx-bridge ;;
    crates/hyperfluxctl/*) echo hyperfluxctl ;;
    integrations/openrgb/* | sdk/cpp/*) echo openrgb-plugin+cpp-sdk ;;
    sdk/python/*) echo python-sdk ;;
    scripts/* | packaging/* | Makefile) echo scripts+packaging ;;
    *) echo other ;;
    esac
}

is_code() {
    case $1 in
    *.patch | */fixtures/* | *.md | */vendor/* | */third_party/*) return 1 ;;
    *.rs | *.c | *.h | *.cpp | *.hpp | *.cc | *.py | *.sh | *.mk | *.cmake | *.spec) return 0 ;;
    Makefile | */Makefile | */Kbuild | */CMakeLists.txt | */PKGBUILD | */debian/rules) return 0 ;;
    */debian/*.postinst | */debian/*.prerm | */debian/*.postrm | */completions/*) return 0 ;;
    esac
    [ "$(head -c 2 "$1")" = '#!' ]
}

while IFS= read -r -d '' file; do
    if [ ! -f "$file" ] || ! is_code "$file"; then continue; fi
    name=$(part "$file")
    lines[$name]=$(( ${lines[$name]:-0} + $(grep -cv '^[[:space:]]*$' "$file" || true) ))
done < <(git ls-files -z --cached --others --exclude-standard)

status=0 total=0
for name in "${!budget[@]}" other; do
    count=${lines[$name]:-0} limit=${budget[$name]:-}
    total=$((total + count))
    if [ -n "$limit" ] && [ "$count" -gt "$limit" ]; then status=1; fi
done
[ "$total" -le "$total_budget" ] || status=1

printf '%-24s %7s %7s\n' part lines budget
for name in "${!budget[@]}" other; do
    count=${lines[$name]:-0} limit=${budget[$name]:-}
    printf '%-24s %7d %7s%s\n' "$name" "$count" "${limit:--}" \
        "$([ -n "$limit" ] && [ "$count" -gt "$limit" ] && echo '  over budget')"
done | sort
printf '%-24s %7d %7d\n' total "$total" "$total_budget"
exit "$status"
