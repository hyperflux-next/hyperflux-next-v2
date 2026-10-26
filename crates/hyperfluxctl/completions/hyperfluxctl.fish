# SPDX-License-Identifier: GPL-2.0-only
# fish completion for hyperfluxctl

set -l commands status devices battery watch doctor bench capture stall-check report support-bundle probe help
set -l none "not __fish_seen_subcommand_from $commands"

complete -c hyperfluxctl -f
complete -c hyperfluxctl -n $none -a status -d 'Driver, bridge and device summary'
complete -c hyperfluxctl -n $none -a devices -d 'Details of every paired device'
complete -c hyperfluxctl -n $none -a battery -d 'Battery levels'
complete -c hyperfluxctl -n $none -a watch -d 'Print device changes as they happen'
complete -c hyperfluxctl -n $none -a doctor -d 'Find problems and say what to do'
complete -c hyperfluxctl -n $none -a bench -d 'Measure report latency of every online device (read-only)'
complete -c hyperfluxctl -n $none -a capture -d 'Record receiver reports to a file'
complete -c hyperfluxctl -n $none -a stall-check -d 'Look for cursor stalls while the mouse moves'
complete -c hyperfluxctl -n $none -a report -d 'Make a full device report file'
complete -c hyperfluxctl -n $none -a support-bundle -d 'Write a file for a bug report'
complete -c hyperfluxctl -n $none -a probe -d 'Ask the receiver about its paired devices (diagnostic)'
complete -c hyperfluxctl -n $none -a help -d 'Show usage'
complete -c hyperfluxctl -n $none -l help -d 'Show usage'
complete -c hyperfluxctl -n $none -l version -d 'Show the version'

complete -c hyperfluxctl -l json -d 'Print JSON'
complete -c hyperfluxctl -l socket -r -F -d 'Bridge socket'

complete -c hyperfluxctl -n '__fish_seen_subcommand_from battery' -l waybar -d 'Print JSON for waybar'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from doctor' -l explain -d 'Explain every finding'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from doctor' -l fix -d 'Offer the safe fixes'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from doctor' -l yes -d 'Run user-level fixes without asking'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from bench' -l device -x -d 'Measure only this device id'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from bench' -l delay-us -x -d 'First GET delay in microseconds'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from bench' -l retry-us -x -d 'Wait between GETs in microseconds'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from bench' -l iterations -x -d 'Runs per delay'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from bench' -l query -x -a 'firmware battery' -d 'Query to send'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from bench' -l reread-busy -d 'Read again after a busy answer'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from bench' -l quick -d 'Only the most useful waits'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from bench' -l defaults -d 'Only the timings the bridge uses'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from capture stall-check report' -l seconds -x -d 'How long to record'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from stall-check' -l threshold-ms -x -d 'Shortest gap that counts'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from stall-check' -l ab -d 'Send queries in the second half'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from capture report support-bundle' -l output -r -F -d 'Output file'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from support-bundle' -l preview -d 'Show what would be written'
complete -c hyperfluxctl -n '__fish_seen_subcommand_from probe' -a receiver -d 'The receiver itself'
