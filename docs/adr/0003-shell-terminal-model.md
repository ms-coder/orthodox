# ADR-0003: Command line and terminal: one persistent shell per window

Status: accepted · 2026-11-14 · gaps filled by T4.0 (2026-11-16); frozen for T4.1/T4.2/T4.3

## Context

F11/F12: a command line under the panels that runs in the active panel's directory with the user's
shell, visible command output, Ctrl+O to hide the panels and see the terminal full size, and panel
directory and shell cwd staying in sync. Running each command in a fresh subprocess loses shell state
(aliases, env, `cd`, jobs) and cannot host interactive programs.

## Decision

- One persistent shell per main window, hosted in a VTE terminal (`vte4`). The panels overlay /
  stack above it; Ctrl+O (`app.terminal_toggle`) shows the terminal full size.
- The command line widget is a GTK entry; Enter sends the text with `feed_child`. A *plain*
  `cd` (`ofm_core::cmdline::parse_cd_target`) never reaches the shell: OFM navigates the panel
  itself, and the next panel→shell sync aligns the shell. `cd -` and anything not resolvable
  purely (`cd -P x`, variables) goes to the shell; its OSC 7 reply moves the panel.
- Quoting for inserted names (Ctrl+Enter, Ctrl+F) uses `shell_quote` on bytes.
- Terminal editors (`vim`, `nano`, `mcedit`, F4 with `editor.terminal_editors`) run inside the
  terminal area but NOT through the shell (changed in T5.5b after the M5 review): the editor is
  spawned as its own VTE child with an exact argv via `TerminalHandle::run_program`, replacing the
  shell view until it exits, then the panels return. Nothing is typed into the shell, so there is
  no quoting, half-typed-line or Idle-detection problem and it works for every shell kind.

### Spawning and rc injection (OSC 7 emission)

The shell comes from `[terminal] shell` (empty = `$SHELL`, fallback `/bin/sh` if unset), spawned
in the active panel's directory, with the terminal's environment plus `TERM=xterm-256color`.
Detection is by basename of the shell path:

- **bash**: `bash --rcfile <wrapper>`. The wrapper (in `resources/shell/`, written per session or
  embedded) returns early for non-interactive shells, sources `~/.bashrc` if readable, sets
  `HISTCONTROL="ignorespace${HISTCONTROL:+:$HISTCONTROL}"`, and sets
  `PROMPT_COMMAND="__ofm_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"` where `__ofm_osc7` prints
  `ESC ] 7 ; file://HOST/ENCODED-PWD ST` (appended so user hooks keep working).
- **zsh**: `ZDOTDIR=<wrapper-dir>` containing a `.zshrc` that sources `~/.zshrc` (via
  `.zshenv`/`.zprofile` pass-through files in the wrapper dir; `OFM_ORIG_ZDOTDIR` carries the
  user's own `ZDOTDIR`, which the wrapper `.zshrc` restores before sourcing theirs) and registers
  `precmd_functions+=(__ofm_osc7)` plus `chpwd_functions+=(__ofm_osc7)` (no clobbering of user
  hooks; `HIST_IGNORE_SPACE` note: zsh ignores leading-space commands only when that option is
  set, so the wrapper enables it if unset).
- **fish**: `fish --init-command <snippet>` defining `__ofm_osc7`, then wraps the prompt:
  `functions -c fish_prompt __ofm_ofm_prompt; function fish_prompt; __ofm_ofm_prompt; __ofm_osc7; end`
  plus `function __ofm_osc7_pwd --on-variable PWD; __ofm_osc7; end`. Modern fish emits OSC 7 by
  itself when it detects a supporting terminal; double emission is harmless (idempotent parse).
- **Anything else** (sh, dash, csh, …): plain spawn, **no injection, no sync in either
  direction** — without idle detection an injected `cd` could land in a running program's stdin,
  so OFM never sends one. OSC 7 arriving anyway (a user rc that emits it) is still parsed and
  moves the panel: parse-always, inject-never. Documented in the manual.

The OSC 7 payload is `file://HOST/percent-encoded-path` (VTE delivers it as
`current-directory-uri`): percent-encode every path byte outside the RFC 3986 unreserved set
(`ALPHA DIGIT - . _ ~`) plus `/`; uppercase `%XX`. Reference implementation:
`ofm_core::cmdline::osc7_encode`/`osc7_parse` (round-trip proptested on arbitrary bytes). The
rc snippets' shell-side encoders must agree with it (T4.3 tests the Rust side against fixtures;
T4.1 tests the snippets with real shells).

### Idle/busy detection (`ShellState`: Idle | Busy | Exited)

- Spawn → **Busy**; the first OSC 7 → **Idle** (so a panel navigation during shell start-up never
  injects a `cd` into the rc phase; fed bytes would merely queue in the pty anyway).
- Every line OFM feeds (command line Enter, a sync `cd`, an F4 editor) → **Busy**; the next
  OSC 7 → **Idle**.
- A `commit` containing `\r` on the VTE widget (the user typed Enter directly into the terminal)
  → **Busy** until the next OSC 7 — this is what keeps cd sync from injecting into a program the
  user launched by typing in the terminal view.
- Child exit → **Exited**. The terminal prints a short notice into the scrollback
  ("[process exited — restarting shell]") and respawns immediately (same rules as the first
  spawn: active panel dir, rc injection); every exit is logged. Three exits within ~2 s stop the
  respawning (a notice says so; the shell restarts on the next Ctrl+O or keypress in the
  terminal; T4.5).
- OSC 7 is only a heuristic: any program that prints `ESC ] 7 ; file://… ST` (e.g. `cat` of a
  hostile file) can fake it, moving the panel and marking the shell Idle. Idle detection
  therefore cannot be relied on for security; what keeps a fed line harmless is byte-exact,
  shell-specific quoting (`QuoteStyle`: fish differs from POSIX), refusing to feed names with
  control bytes, and not feeding into a half-typed terminal line (T4.5).
- If in doubt OFM treats the shell as Busy: a stalled sync is harmless, a wrong injection is not
  (e.g. no OSC 7 for an unknown shell ⇒ never Idle-for-injection; panel→shell sync disabled).

### cd sync rules

- **Panel → shell**: on `PanelEvent::Location` of the active tab, on panel/tab switches, when the
  terminal becomes visible, and at spawn. Precondition: shell `Idle`, `!cmdline_has_text`
  (the user is composing a command), panel location `Local`, sync-capable shell (rc injected),
  and the shell's known cwd (last OSC 7) differs from the panel dir. Then feed the exact bytes
  `ofm_core::cmdline::cd_command(dir)` = ` cd -- '<quoted>'\n` — leading space so it stays out of
  history (`ignorespace` is set by the wrapper), `--` so option-like paths are safe, byte-exact
  quoting so non-UTF-8 directories work (invariant 4).
- **Shell → panel**: on an accepted OSC 7 (host empty, `localhost`, or the machine's own
  hostname — fetched once off the main thread at start-up and cached; other hosts are ignored)
  whose decoded path differs from the active panel's directory: navigate the active panel
  through `navigation::goto::navigate` (history, tab-lock and "cursor on the dir we came from"
  rules included). Navigating only on a difference is the fixed point that stops the loop: our
  own sync `cd` answers with OSC 7 == panel dir ⇒ no navigation ⇒ no new Location event.
- Rapid switches coalesce: at most one sync `cd` in flight; a Location event arriving while Busy
  just marks the shell's cwd stale and re-syncs at the next OSC 7.
- Non-local panel locations (archives, GVfs) never sync; switching to such a tab leaves the
  shell where it is.

### Ctrl+O (`app.terminal_toggle`) semantics

Toggles **only the panels area** (`win.panels` visibility): hidden, the terminal expands into its
space (the `terminal_slot` content is always present; showing is not re-spawning). The command
line row, menu bar and F-key bar stay visible (Far behaviour: read output and keep typing
commands); focus that was on a now-hidden panel moves to the command line entry. Running a
command from the command line calls `TerminalHandle::show()` (output must be visible); when the
panels come back after the command ends is governed by a setting
(`[terminal] return_to_panels`, default: never — Ctrl+O brings them back; the key lands with the
lead before T4.1 starts, see OWNERSHIP.md "M4 lead follow-ups"). Ctrl+O during a running command
or editor just toggles the view; nothing is suspended.

### Tab rule

With text in the command line, Tab is `cmdline.complete` (bound in the `[cmdline]` keymap section
of all three presets since T4.0 — that section applies only while the line is non-empty, which
`AppContext::set_cmdline_has_text` drives). With an **empty** command line the `[cmdline]`
section is inactive, so Tab falls through to `[panels]` = `panel.switch`. Completion lists the
directory of the word being completed **on the I/O pool** (it may differ from the panel's) and
matches with `ofm_core::cmdline::complete_in` (byte-exact, unique match appends `/` for
directories, ambiguous matches extend by the common prefix and list the candidates).

### `typing` modes and key routing (ADR-0005)

- `command_line` (Far/mc default): plain printable keys that no command claimed focus the command
  line and are typed there (the cmdline module's bubble-phase controller runs *after*
  quicksearch's, which in this mode claims only Alt+letter). Ctrl+E/… history keys work from the
  panels; the entry keeps normal text-editing keys (`dispatch::entry_wants_key`).
- `quick_search` (TC): plain printable keys go to quick search (existing controller); the command
  line is reached by clicking it or with the history keys; once focused, typing stays in it.
- While the **VTE widget has focus** (the user clicked the terminal / an F4 editor runs), only
  the `[global]` keymap section is consulted — arrows, letters, Enter, Tab go to the shell, but
  Ctrl+O, F10, Ctrl+J still work. Needs a 3-line frozen-core extension (see below).
- `Enter` on a non-empty line is `cmdline.execute`; on an empty line the `[cmdline]` section is
  inactive and Enter stays `panel.enter_dir` (open the entry under the cursor). `Esc` on a
  non-empty line clears it (`cmdline.clear`), otherwise `panel.cancel`.

## Alternatives considered

- Spawn `sh -c` per command and capture output: loses shell state, no interactivity, no TTY programs.
- Own terminal emulation: large, error-prone; VTE exists and is on every GNOME system.
- Separate terminal window: breaks the Far/mc workflow of panels over terminal.
- Always parse `cd` and navigate panels without shell confirmation: wrong for `cd -`, variables
  and options — resolved targets navigate directly, everything else defers to the shell + OSC 7.

## Consequences

- Depends on shell cooperation (rc snippet); documented limits for exotic shells (no sync).
- Idle detection is heuristic (fed lines + OSC 7 freshness + Enter-in-terminal); if unsure OFM
  does not send `cd`.
- VTE is app-only (invariant 1); `ofm-core::cmdline` holds all testable logic.
- **M4 lead follow-ups** (small frozen-core/config changes the lead applies right after merging
  T4.0, before T4.1/T4.2 start; recorded in `crates/ofm-app/OWNERSHIP.md`):
  1. `dispatch::resolve`/`install_key_controller`: when the focus is a `vte::Terminal`, resolve
     chords in `Global` only (the VTE is a text-entry-like surface, invariant 5's documented
     exception).
  2. `ofm-config`: add `[terminal] return_to_panels` (bool, default `false`) +
     `defaults/config.toml` documentation.
- Interfaces frozen by T4.0: `ofm_core::cmdline` (quote/osc7/cd/complete/history), the app
  modules' hooks and `terminal::{ShellState, ShellHost, TerminalHandle}` /
  `cmdline::CmdlineState` in `ctx.ext`.
