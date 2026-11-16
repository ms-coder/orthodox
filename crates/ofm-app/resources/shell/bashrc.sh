# OFM terminal integration for bash (ADR-0003). OFM spawns the shell as
#   bash --rcfile <this file>
# This file defines __ofm_osc7 (reports the working directory to OFM with OSC 7, encoded
# exactly like ofm_core::cmdline::osc7_encode), then — for interactive shells only —
# sources ~/.bashrc, keeps space-prefixed commands out of the history (OFM feeds its
# panel->shell `cd` syncs with a leading space) and prepends __ofm_osc7 to PROMPT_COMMAND
# (appended first, so user hooks keep running).
# The encoder is defined *before* the interactive guard so OFM's tests can source this file
# non-interactively and call __ofm_osc7 directly.

__ofm_osc7() {
    # Byte-wise iteration and encoding, exactly like osc7_encode: `local LC_ALL=C`
    # makes ${#PWD} and ${PWD:i:1} operate on single bytes (valid UTF-8 sequences
    # fall apart into their bytes, which are each percent-encoded — precisely what
    # osc7_encode does with the raw path bytes). Every byte outside [A-Za-z0-9._~/-]
    # becomes %XX (uppercase hex); the host part is empty (always accepted by OFM).
    local LC_ALL=C
    local i c cp out
    out=''
    for (( i = 0; i < ${#PWD}; i++ )); do
        c=${PWD:i:1}
        case $c in
            [A-Za-z0-9._~/-]) out+=$c ;;
            *)
                printf -v cp '%d' "'$c"
                if (( cp < 0 )); then
                    (( cp += 256 )) # invalid byte reported signed by the libc
                fi
                printf -v out '%s%%%02X' "$out" "$cp"
                ;;
        esac
    done
    printf '\033]7;file://%s%s\033\\' '' "$out"
}

# Everything below is for interactive shells only.
case $- in
    *i*) ;;
    *) return 0 2>/dev/null || exit 0 ;;
esac

[ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"

HISTCONTROL="ignorespace${HISTCONTROL:+:$HISTCONTROL}"
PROMPT_COMMAND="__ofm_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
