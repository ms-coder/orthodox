# OFM terminal integration for zsh (ADR-0003): .zshenv passthrough.
# OFM spawns zsh with ZDOTDIR pointing at a per-session directory, so zsh reads *this* file
# instead of the user's ~/.zshenv. Source the user's own file from $OFM_ORIG_ZDOTDIR (their
# ZDOTDIR when they had one) or $HOME, then make sure ZDOTDIR still points at the wrapper
# directory: users commonly set `ZDOTDIR` in ~/.zshenv itself, which would otherwise skip our
# .zshrc (and with it the OSC 7 integration). The user's value is remembered in
# OFM_ORIG_ZDOTDIR; the wrapper .zshrc restores it before the user's .zshrc runs.

typeset -g __ofm_zdotdir=$ZDOTDIR
[ -r "${OFM_ORIG_ZDOTDIR:-$HOME}/.zshenv" ] && . "${OFM_ORIG_ZDOTDIR:-$HOME}/.zshenv"
if [[ $ZDOTDIR != "$__ofm_zdotdir" ]]; then
    export OFM_ORIG_ZDOTDIR=$ZDOTDIR
    export ZDOTDIR=$__ofm_zdotdir
fi
unset __ofm_zdotdir
