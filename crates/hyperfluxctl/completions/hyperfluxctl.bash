# SPDX-License-Identifier: GPL-2.0-only
# bash completion for hyperfluxctl

_hyperfluxctl() {
    local cur prev words cword
    _init_completion || return

    case $prev in
        --socket | --output)
            _filedir
            return
            ;;
        --query)
            COMPREPLY=($(compgen -W 'firmware battery' -- "$cur"))
            return
            ;;
        --device | --delay-us | --retry-us | --iterations | --seconds | --threshold-ms)
            return
            ;;
    esac

    local command='' word
    for word in "${words[@]:1:cword-1}"; do
        case $word in
            status | devices | battery | watch | doctor | bench | capture | stall-check | \
                report | support-bundle | probe | help)
                command=$word
                break
                ;;
        esac
    done

    local options
    case $command in
        '')
            COMPREPLY=($(compgen -W 'status devices battery watch doctor bench capture
                stall-check report support-bundle probe help --json --socket --help --version' -- "$cur"))
            return
            ;;
        battery) options='--waybar' ;;
        doctor) options='--explain --fix --yes' ;;
        bench) options='--device --delay-us --retry-us --iterations --query --reread-busy --quick --defaults' ;;
        capture) options='--seconds --output' ;;
        stall-check) options='--seconds --threshold-ms --ab' ;;
        report) options='--seconds --output' ;;
        support-bundle) options='--preview --output' ;;
        probe) options='receiver' ;;
    esac
    COMPREPLY=($(compgen -W "$options --json --socket" -- "$cur"))
}

complete -F _hyperfluxctl hyperfluxctl
