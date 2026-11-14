# ADR-0005: Config, keymap, theme and session file formats

Status: accepted · 2026-11-14

## Context

F29/F30/F33: everything configurable by TOML, three keymap presets, three themes, live theme reload,
session restore. Files must survive partial edits, wrong values and version changes without crashing.

## Decision

**Layout.** `$XDG_CONFIG_HOME/ofm/` (default `~/.config/ofm/`): `config.toml`, `keymap.toml`, `menu.toml`,
`themes/*.toml`. `$XDG_STATE_HOME/ofm/` (default `~/.local/state/ofm/`): `session.toml`, `history.toml`.
Built-in defaults are embedded with `include_str!` from `crates/ofm-config/defaults/` (the files there are
the normative examples: `config.toml`, `menu.toml`, `keymap-{far,tc,mc}.toml`, `themes/*.toml`).

**Rules.** Every file has `version = 1`; unknown keys are ignored (forward compatibility) and every key
is optional (`serde(default)`). An invalid user file -> built-in defaults + a `Warning` shown by the
UI; never a crash. Writes are atomic (temp + fsync + rename). Paths in state files are stored as UTF-8
`path` plus raw `path_bytes` for non-UTF-8 paths (paths are bytes).

**Key chords.** `Mod+Mod+Key`, modifiers `Ctrl Alt Shift Super` (canonical output order), key =
one printable char (letters case-insensitive), `F1`..`F24`, `Up Down Left Right PgUp PgDn Home End Enter
Esc Tab Space Backspace Del Ins Menu`, keypad `Gray+ Gray- Gray* Gray/` (the Unicode minus is accepted as
alias) and `KP0`..`KP9`, `KP.`. Examples: `Ctrl+Shift+F5`, `Alt+=`, `Ctrl+\`, `Ctrl+,`. The parser splits at
the last `+` that is followed by a non-empty key name, so `Ctrl++` is not valid (use `Ctrl+Gray+`).

**Keymap.** Sections are contexts: `[global] [panels] [cmdline] [viewer] [dialog]`; entries are
`"<chord>" = "<command id>"` (many chords may map to one command). Top level: `preset`, `typing`
(`command_line` = Far/mc: printable keys go to the command line, Alt+letter = quick search;
`quick_search` = TC: letters search). Resolution: modal dialog or jobs view -> `dialog`; viewer window ->
`viewer`; main window -> `cmdline` (only while the command line is non-empty) -> `panels` -> `global`;
`viewer`/`dialog` fall back to `global`. A user `keymap.toml` is merged over the selected preset; same
chord in the same section replaces; `"<chord>" = ""` unbinds. Conflicts (same chord twice in a context,
or in both `global` and `panels`) are reported as warnings; the last definition wins. Text-entry
widgets keep their own editing keys (the dispatcher passes them through). A test requires every
`CommandId` (see `docs/COMMANDS.md`, generated into the presets) to be bound in every preset.

**Themes.** One file per theme: `id`, `name`, `base = "fixed" | "adwaita"`, `[font]`, `[retro]`
(`monospace_grid`, `frames = none|hairline|single|double`, `rounded_corners`, `animations`), semantic
colour tables `[colors.{panel,cursor,marked,status,cmdline,fkey,viewer,dialog,progress,error}]`, and
ordered `[[highlight]]` rules (`attr` and/or `mask` globs, `fg`, `bg`, `bold`, `icon`; first match wins).
Colours are `#RRGGBB[AA]`, or `@accent|@dim|@error|@success|@warning` references to the system palette,
or empty (unset). `base = "adwaita"` derives unspecified roles from `adw::StyleManager`. Built-ins:
`far-blue`, `commander-light`, `adwaita`; user themes in `themes/` override by `id`; the directory is
watched (`notify`) for instant reload. The app converts a theme into a render palette (custom widgets)
and generated CSS (standard widgets); GNOME high-contrast forces the Adwaita base.

## Alternatives considered

- JSON/YAML: no comments / indentation traps. RON/KDL: unfamiliar to users.
- One keymap file with `[bindings."Ctrl+F5"]` command->keys: harder to detect chord conflicts and to read.
- GSettings/dconf: not portable beyond GNOME (see ADR-0001 porting goal), poor for large structured data.

## Consequences

- Keymap presets are data, regenerable from the command registry; adding a command means touching
  `commands.rs`, `COMMANDS.md` and three preset files (enforced by tests).
- Ignoring unknown keys means typos are silent; T1.3 may log them at `debug` level.
- Future plugin commands (v2.0) register ids at runtime; the context sections already allow it.
