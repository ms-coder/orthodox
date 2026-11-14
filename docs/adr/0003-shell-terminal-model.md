# ADR-0003: Command line and terminal: one persistent shell per window

Status: accepted · 2026-11-14

## Context

F11/F12: a command line under the panels that runs in the active panel's directory with the user's
shell, visible command output, Ctrl+O to hide the panels and see the terminal full size, and panel
directory and shell cwd staying in sync. Running each command in a fresh subprocess loses shell state
(aliases, env, `cd`, jobs) and cannot host interactive programs.

## Decision

- One persistent `$SHELL` per main window, hosted in a VTE terminal (`vte4`). The panels overlay /
  stack above it; Ctrl+O (`app.terminal_toggle`) shows the terminal full size.
- The shell is spawned with an injected rc snippet emitting OSC 7 (`file://host/path`) on every prompt:
  bash via `--rcfile` wrapper that sources `~/.bashrc`; zsh via a `ZDOTDIR` wrapper that sources the
  user's files; fish emits OSC 7 natively. Other shells: no cwd follow (panel->shell sync still works).
- Panel follows shell: VTE `current-directory-uri-changed` / OSC 7 -> `ofm_core::cmdline::osc7_to_path` ->
  navigate the active panel. Shell follows panel: when the panel's directory changes and the shell is
  idle (no foreground job; tracked by prompt markers), send ` cd -- '<quoted path>'` + Enter, with a
  leading space so it stays out of history (HISTCONTROL=ignorespace is set by the snippet).
- Command line widget is a GTK entry; Enter sends the text with `feed_child`, switching to the
  terminal view when the command is not a plain `cd` (handled by `parse_cd_target`, which only changes
  the panel). Output is visible in the terminal; panels reappear on Ctrl+O or when the command ends
  (setting, default: stay until Ctrl+O).
- Terminal editors (`vim`, `nano`, `mcedit`, F4) run inside this terminal.
- Quoting for inserted names (Ctrl+Enter, Ctrl+F) uses `shell_quote` on bytes.

## Alternatives considered

- Spawn `sh -c` per command and capture output: loses shell state, no interactivity, no TTY programs.
- Own terminal emulation: large, error-prone; VTE exists and is on every GNOME system.
- Separate terminal window: breaks the Far/mc workflow of panels over terminal.

## Consequences

- Depends on shell cooperation (rc snippet); documented limits for exotic shells.
- Idle detection is heuristic (prompt marker + OSC 7 freshness); if unsure OFM does not send `cd`.
- VTE is app-only (invariant 1); `ofm-core::cmdline` holds all testable logic.
