# OFM terminal integration for zsh (ADR-0003): .zprofile passthrough.
# zsh reads .zprofile of login shells from ZDOTDIR, which is OFM's wrapper directory while the
# shell starts; source the user's own file (see zshenv.zsh for the variables).

[ -r "${OFM_ORIG_ZDOTDIR:-$HOME}/.zprofile" ] && . "${OFM_ORIG_ZDOTDIR:-$HOME}/.zprofile"
