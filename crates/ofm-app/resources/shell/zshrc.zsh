# OFM terminal integration for zsh (ADR-0003). OFM spawns the shell with ZDOTDIR pointing
# at a per-session directory whose only file is this .zshrc (zsh still reads ~/.zshenv and
# ~/.zprofile from $HOME; a user rc that resets ZDOTDIR disables the integration — OFM then
# simply never sees OSC 7 and never injects a cd).
# For interactive shells this file sources ~/.zshrc, enables HIST_IGNORE_SPACE (zsh only
# skips space-prefixed commands when it is set; OFM feeds its panel->shell `cd` syncs with
# a leading space) and appends __ofm_osc7 to precmd_functions and chpwd_functions (user
# hooks keep running; OSC 7 is emitted on every prompt and every directory change).
# The encoder is defined *before* the interactive guard so OFM's tests can source this file
# non-interactively and call __ofm_osc7 directly.

__ofm_osc7() {
    # Byte-wise iteration and encoding, exactly like osc7_encode: NO_MULTIBYTE makes
    # $#PWD and $PWD[i] operate on single bytes (valid UTF-8 sequences fall apart into
    # their bytes, which are each percent-encoded — precisely what osc7_encode does with
    # the raw path bytes). Every byte outside [A-Za-z0-9._~/-] becomes %XX (uppercase
    # hex); the host part is empty (always accepted).
    emulate -L zsh
    setopt localoptions NO_MULTIBYTE
    local i c cp out
    out=''
    for (( i = 1; i <= $#PWD; i++ )); do
        c=$PWD[i]
        case $c in
            [A-Za-z0-9._~/-]) out+=$c ;;
            *)
                cp=$(printf '%d' "'$c")
                if (( cp < 0 )); then
                    (( cp += 256 )) # invalid byte reported signed by the libc
                fi
                out+=$(printf '%%%02X' $cp)
                ;;
        esac
    done
    printf '\033]7;file://%s%s\033\\' '' "$out"
}

# Everything below is for interactive shells only.
[[ -o interactive ]] || return 0

[ -r "$HOME/.zshrc" ] && . "$HOME/.zshrc"

setopt HIST_IGNORE_SPACE

typeset -ga precmd_functions
typeset -ga chpwd_functions
precmd_functions+=(__ofm_osc7)
chpwd_functions+=(__ofm_osc7)
