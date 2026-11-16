# OFM terminal integration for fish (ADR-0003). OFM spawns the shell as
#   fish --init-command <contents of this file>
# (evaluated after the user's config.fish, before the first prompt). It defines __ofm_osc7
# (reports the working directory to OFM with OSC 7, encoded exactly like
# ofm_core::cmdline::osc7_encode), wraps fish_prompt to emit it on every prompt and also
# emits it on every PWD change. Modern fish emits OSC 7 by itself on supporting terminals;
# double emission is harmless (OFM's parse is idempotent). fish never records space-prefixed
# commands in the history, so no history option is set.

function __ofm_osc7
    # fish's url-style escape keeps the RFC 3986 unreserved characters and '/' literal
    # and percent-encodes every other byte as uppercase %XX — exactly what
    # ofm_core::cmdline::osc7_encode produces for the path (including non-UTF-8 bytes).
    # The host part is empty (always accepted by OFM).
    printf '\e]7;file://%s\e\\' (string escape --style=url -- $PWD)
end

functions -c fish_prompt __ofm_ofm_prompt
function fish_prompt
    __ofm_ofm_prompt
    __ofm_osc7
end

function __ofm_osc7_pwd --on-variable PWD
    __ofm_osc7
end
