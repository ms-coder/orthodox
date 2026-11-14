//! Keymaps: presets + user overrides, lookup by context, conflict detection.
//!
//! File format (ADR-0005): top-level `version`, `preset`, `typing`; sections `[global] [panels]
//! [cmdline] [viewer] [dialog]` with `"<chord>" = "<command id>"` entries. A user `keymap.toml`
//! has the same shape and is merged over the selected preset: same chord in the same section
//! replaces the preset binding; `"<chord>" = ""` unbinds it. The preset itself is selected by
//! `config.toml` (`[keymap] preset`); a `preset` key inside `keymap.toml` is ignored.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ofm_core::{CommandId, KeyChord, KeyContext, commands};

use crate::defaults;
use crate::error::{ConfigError, Warning};
use crate::io;
use crate::paths::ConfigPaths;

/// Built-in presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Far Manager (default).
    Far,
    /// Total / Double Commander.
    Tc,
    /// Midnight Commander.
    Mc,
}

impl Preset {
    /// Parse the `config.toml` spelling (`far`, `tc`, `mc`).
    pub fn from_name(name: &str) -> Option<Preset> {
        match name {
            "far" => Some(Preset::Far),
            "tc" => Some(Preset::Tc),
            "mc" => Some(Preset::Mc),
            _ => None,
        }
    }
    fn source(self) -> (&'static str, &'static str) {
        match self {
            Preset::Far => ("<builtin keymap-far.toml>", defaults::KEYMAP_FAR),
            Preset::Tc => ("<builtin keymap-tc.toml>", defaults::KEYMAP_TC),
            Preset::Mc => ("<builtin keymap-mc.toml>", defaults::KEYMAP_MC),
        }
    }
}

/// What printable keys do in the panels context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingMode {
    /// Far/mc: letters go to the command line, Alt+letter = quick search.
    CommandLine,
    /// TC: letters start quick search.
    QuickSearch,
}

impl TypingMode {
    /// Parse the file spelling (`command_line`, `quick_search`).
    pub fn from_name(name: &str) -> Option<TypingMode> {
        match name {
            "command_line" => Some(TypingMode::CommandLine),
            "quick_search" => Some(TypingMode::QuickSearch),
            _ => None,
        }
    }
}

/// A binding that was overridden or ambiguous, reported on load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// Context section.
    pub context: KeyContext,
    /// The chord.
    pub chord: KeyChord,
    /// Commands that claimed it (last one wins).
    pub commands: Vec<CommandId>,
}

/// Resolved keymap.
#[derive(Debug, Clone)]
pub struct Keymap {
    /// Typing behaviour.
    pub typing: TypingMode,
    bindings: HashMap<(KeyContext, KeyChord), CommandId>,
    /// Same chord claimed twice inside one file section (e.g. `ctrl+a` and `Ctrl+A`).
    duplicates: Vec<Conflict>,
}

/// One parsed `"<chord>" = "<command>"` line; `None` command = unbind.
struct Entry {
    ctx: KeyContext,
    chord: KeyChord,
    cmd: Option<CommandId>,
}

/// A keymap file after parsing, before merging.
struct Parsed {
    typing: Option<TypingMode>,
    entries: Vec<Entry>,
}

fn warn(origin: &str, message: String) -> Warning {
    Warning {
        file: Some(PathBuf::from(origin)),
        message,
    }
}

fn parse_file(text: &str, origin: &str) -> Result<(Parsed, Vec<Warning>), ConfigError> {
    let table: toml::Table = toml::from_str(text).map_err(|e| ConfigError::Parse {
        path: PathBuf::from(origin),
        message: e.to_string(),
    })?;
    let mut warnings = Vec::new();
    let mut typing = None;
    match table.get("typing").and_then(toml::Value::as_str) {
        None | Some("") => {}
        Some(t) => match TypingMode::from_name(t) {
            Some(m) => typing = Some(m),
            None => warnings.push(warn(origin, format!("unknown typing mode {t:?}"))),
        },
    }
    let mut entries = Vec::new();
    for (section, value) in &table {
        let Some(ctx) = KeyContext::from_section(section) else {
            continue;
        };
        let Some(keys) = value.as_table() else {
            warnings.push(warn(origin, format!("[{section}] is not a table")));
            continue;
        };
        for (chord_text, cmd_value) in keys {
            let chord = match chord_text.parse::<KeyChord>() {
                Ok(c) => c,
                Err(e) => {
                    warnings.push(warn(origin, format!("[{section}] {e}")));
                    continue;
                }
            };
            let Some(cmd_text) = cmd_value.as_str() else {
                warnings.push(warn(
                    origin,
                    format!("[{section}] {chord_text}: command must be a string"),
                ));
                continue;
            };
            let cmd = if cmd_text.is_empty() {
                None
            } else {
                match CommandId::lookup(cmd_text) {
                    Some(c) => Some(c),
                    None => {
                        warnings.push(warn(
                            origin,
                            format!("[{section}] {chord_text}: unknown command {cmd_text:?}"),
                        ));
                        continue;
                    }
                }
            };
            entries.push(Entry { ctx, chord, cmd });
        }
    }
    Ok((Parsed { typing, entries }, warnings))
}

impl Keymap {
    fn empty() -> Keymap {
        Keymap {
            typing: TypingMode::CommandLine,
            bindings: HashMap::new(),
            duplicates: Vec::new(),
        }
    }

    /// Apply a parsed file on top of `self`. Chords that appear twice within the file (after
    /// normalisation) are recorded as duplicates; the last one wins.
    fn apply(&mut self, parsed: Parsed) {
        let mut claimed: HashMap<(KeyContext, KeyChord), Vec<CommandId>> = HashMap::new();
        for e in &parsed.entries {
            if let Some(c) = e.cmd {
                claimed.entry((e.ctx, e.chord)).or_default().push(c);
            }
        }
        for ((context, chord), mut commands) in claimed {
            commands.dedup();
            if commands.len() > 1 {
                self.duplicates.push(Conflict {
                    context,
                    chord,
                    commands,
                });
            }
        }
        for e in parsed.entries {
            match e.cmd {
                Some(c) => {
                    self.bindings.insert((e.ctx, e.chord), c);
                }
                None => {
                    self.bindings.remove(&(e.ctx, e.chord));
                }
            }
        }
        self.duplicates.sort_by_key(|c| (c.context, c.chord));
        if let Some(t) = parsed.typing {
            self.typing = t;
        }
    }

    fn conflict_warnings(&self, origin: &str) -> Vec<Warning> {
        self.conflicts()
            .into_iter()
            .map(|c| {
                let names: Vec<_> = c.commands.iter().map(|c| c.as_str()).collect();
                warn(
                    origin,
                    format!(
                        "key {} is bound more than once in [{}]: {} (last wins)",
                        c.chord,
                        c.context.section(),
                        names.join(", ")
                    ),
                )
            })
            .collect()
    }

    /// Parse a preset or user keymap file; unknown command ids and bad chords are collected as
    /// warnings and skipped. Syntax errors are returned as `Err`.
    pub fn from_toml(text: &str, origin: &str) -> Result<(Keymap, Vec<Warning>), ConfigError> {
        let (parsed, mut warnings) = parse_file(text, origin)?;
        let mut km = Keymap::empty();
        km.apply(parsed);
        warnings.extend(km.conflict_warnings(origin));
        Ok((km, warnings))
    }

    /// Built-in preset (always valid; a test enforces it).
    pub fn preset(preset: Preset) -> Keymap {
        let (origin, text) = preset.source();
        Keymap::from_toml(text, origin)
            .map(|(k, _)| k)
            .unwrap_or_else(|_| Keymap::empty())
    }

    /// Preset + user `keymap.toml` + `typing` override from config, with warnings. A broken user
    /// file yields the plain preset and a warning.
    pub fn load(
        paths: &ConfigPaths,
        preset: Preset,
        typing: Option<TypingMode>,
    ) -> (Keymap, Vec<Warning>) {
        let mut km = Keymap::preset(preset);
        let mut warnings = Vec::new();
        let file = paths.keymap_file();
        let origin = file.to_string_lossy().into_owned();
        match io::read_optional(&file) {
            Ok(None) => {}
            Err(e) => warnings.push(Warning {
                file: Some(file.clone()),
                message: e.to_string(),
            }),
            Ok(Some(text)) => match parse_file(&text, &origin) {
                Err(e) => warnings.push(Warning {
                    file: Some(file.clone()),
                    message: format!("{e}; using the {preset:?} preset"),
                }),
                Ok((parsed, mut w)) => {
                    let mut merged = km.clone();
                    merged.apply(parsed);
                    warnings.append(&mut w);
                    warnings.extend(merged.conflict_warnings(&origin));
                    km = merged;
                }
            },
        }
        if let Some(t) = typing {
            km.typing = t;
        }
        // Presets must be conflict-free (tested), so every remaining conflict comes from the user file.
        (km, warnings)
    }

    /// Command bound to `chord` in exactly `ctx` (no fallback).
    pub fn lookup(&self, ctx: KeyContext, chord: KeyChord) -> Option<CommandId> {
        self.bindings.get(&(ctx, chord)).copied()
    }

    /// Resolve in `ctx`, then in `Global` (for the viewer and dialog windows).
    pub fn resolve_in(&self, ctx: KeyContext, chord: KeyChord) -> Option<CommandId> {
        self.lookup(ctx, chord)
            .or_else(|| self.lookup(KeyContext::Global, chord))
    }

    /// Dispatcher resolution order for the main window: `CmdLine` (if `cmdline_has_text`), `Panels`,
    /// `Global`. Dialog and Viewer windows resolve in their own section then `Global`.
    pub fn resolve_main(&self, chord: KeyChord, cmdline_has_text: bool) -> Option<CommandId> {
        if cmdline_has_text && let Some(c) = self.lookup(KeyContext::CmdLine, chord) {
            return Some(c);
        }
        self.resolve_in(KeyContext::Panels, chord)
    }

    /// All chords bound to `cmd` (any context), for menus and the F-key bar. Sorted by context,
    /// then chord.
    pub fn chords_for(&self, cmd: CommandId) -> Vec<(KeyContext, KeyChord)> {
        let mut v: Vec<_> = self
            .bindings
            .iter()
            .filter(|(_, c)| **c == cmd)
            .map(|((ctx, chord), _)| (*ctx, *chord))
            .collect();
        v.sort();
        v
    }

    /// Commands that have no binding at all (a test asserts this is empty for the far preset).
    pub fn unbound_commands(&self) -> Vec<CommandId> {
        let bound: HashSet<CommandId> = self.bindings.values().copied().collect();
        commands::ALL
            .iter()
            .map(|c| c.id)
            .filter(|id| !bound.contains(id))
            .collect()
    }

    /// Same chord claimed twice within one context, or by `Global` and `Panels` at once (with
    /// different commands).
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out = self.duplicates.clone();
        for ((ctx, chord), panels_cmd) in &self.bindings {
            if *ctx != KeyContext::Panels {
                continue;
            }
            if let Some(global_cmd) = self.lookup(KeyContext::Global, *chord)
                && global_cmd != *panels_cmd
            {
                out.push(Conflict {
                    context: KeyContext::Panels,
                    chord: *chord,
                    commands: vec![global_cmd, *panels_cmd],
                });
            }
        }
        out.sort_by_key(|c| (c.context, c.chord));
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ofm_core::commands::cmd;

    fn chord(s: &str) -> KeyChord {
        s.parse().unwrap()
    }

    #[test]
    fn every_preset_covers_every_command_without_conflicts() {
        for (name, p) in [("far", Preset::Far), ("tc", Preset::Tc), ("mc", Preset::Mc)] {
            let (text_origin, text) = p.source();
            let (km, warnings) = Keymap::from_toml(text, text_origin).unwrap();
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
            assert!(km.unbound_commands().is_empty(), "{name}");
            assert!(km.conflicts().is_empty(), "{name}: {:?}", km.conflicts());
        }
        assert_eq!(Keymap::preset(Preset::Far).typing, TypingMode::CommandLine);
        assert_eq!(Keymap::preset(Preset::Tc).typing, TypingMode::QuickSearch);
    }

    #[test]
    fn lookup_and_resolution_order() {
        let km = Keymap::preset(Preset::Far);
        let f10 = chord("F10");
        assert_eq!(km.lookup(KeyContext::Global, f10), Some(cmd::APP_QUIT));
        assert_eq!(km.lookup(KeyContext::Panels, f10), None);
        assert_eq!(km.resolve_main(f10, false), Some(cmd::APP_QUIT));
        // Dialog/viewer sections win over Global, and fall back to it.
        assert_ne!(km.resolve_in(KeyContext::Dialog, f10), Some(cmd::APP_QUIT));
        assert_eq!(
            km.resolve_in(KeyContext::Viewer, f10),
            Some(cmd::VIEWER_CLOSE)
        );
        let jobs = chord("Ctrl+J");
        assert_eq!(km.resolve_in(KeyContext::Viewer, jobs), Some(cmd::APP_JOBS));
        let down = chord("Down");
        assert_eq!(km.resolve_main(down, false), Some(cmd::PANEL_CURSOR_DOWN));
        // While the command line has text the cmdline section wins over panels.
        let enter = chord("Enter");
        assert_eq!(km.resolve_main(enter, false), Some(cmd::PANEL_OPEN));
        assert_eq!(km.resolve_main(enter, true), Some(cmd::CMDLINE_EXECUTE));
        // Keys the cmdline section does not bind still reach the panels.
        assert_eq!(km.resolve_main(down, true), Some(cmd::PANEL_CURSOR_DOWN));
        assert!(!km.chords_for(cmd::APP_QUIT).is_empty());
    }

    #[test]
    fn user_overrides_replace_unbind_and_keep_rest() {
        let (mut km, _) = Keymap::from_toml(
            "[global]\n\"F10\" = \"app.quit\"\n\"F1\" = \"app.help\"\n",
            "t",
        )
        .unwrap();
        let (user, w) = parse_file(
            "typing = \"quick_search\"\n[global]\n\"F10\" = \"app.menu\"\n\"F1\" = \"\"\n",
            "u",
        )
        .unwrap();
        assert!(w.is_empty());
        km.apply(user);
        assert_eq!(
            km.lookup(KeyContext::Global, chord("F10")),
            Some(cmd::APP_MENU)
        );
        assert_eq!(km.lookup(KeyContext::Global, chord("F1")), None);
        assert_eq!(km.typing, TypingMode::QuickSearch);
        assert!(km.conflicts().is_empty());
    }

    #[test]
    fn bad_entries_become_warnings() {
        let (km, w) = Keymap::from_toml(
            "[global]\n\"Bogus+Key\" = \"app.quit\"\n\"F10\" = \"no.such\"\n\"F1\" = 5\n\"F2\" = \"app.help\"\n[nonsense]\nx = 1\n",
            "t",
        )
        .unwrap();
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(
            km.lookup(KeyContext::Global, chord("F2")),
            Some(cmd::APP_HELP)
        );
    }

    #[test]
    fn detects_conflicts() {
        let (km, w) = Keymap::from_toml(
            "[global]\n\"ctrl+j\" = \"app.jobs\"\n\"Ctrl+J\" = \"app.help\"\n\"F3\" = \"app.menu\"\n[panels]\n\"F3\" = \"panel.home\"\n",
            "t",
        )
        .unwrap();
        let c = km.conflicts();
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(w.len(), 2);
        // Last definition wins (TOML tables iterate in key order: "Ctrl+J" < "ctrl+j").
        assert!(km.lookup(KeyContext::Global, chord("Ctrl+J")).is_some());
    }

    #[test]
    fn load_merges_user_file_and_survives_garbage() {
        let d = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"));
        let (km, w) = Keymap::load(&paths, Preset::Tc, None);
        assert!(w.is_empty());
        assert_eq!(km.typing, TypingMode::QuickSearch);

        io::atomic_write(&paths.keymap_file(), "[global]\n\"F10\" = \"app.menu\"\n").unwrap();
        let (km, w) = Keymap::load(&paths, Preset::Far, Some(TypingMode::QuickSearch));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(
            km.lookup(KeyContext::Global, chord("F10")),
            Some(cmd::APP_MENU)
        );
        assert_eq!(km.typing, TypingMode::QuickSearch);

        io::atomic_write(&paths.keymap_file(), "[global\nnot toml").unwrap();
        let (km, w) = Keymap::load(&paths, Preset::Far, None);
        assert_eq!(w.len(), 1);
        assert_eq!(
            km.lookup(KeyContext::Global, chord("F10")),
            Some(cmd::APP_QUIT)
        );
    }

    #[test]
    fn preset_names() {
        assert_eq!(Preset::from_name("mc"), Some(Preset::Mc));
        assert_eq!(Preset::from_name("x"), None);
        assert_eq!(
            TypingMode::from_name("quick_search"),
            Some(TypingMode::QuickSearch)
        );
        assert_eq!(TypingMode::from_name(""), None);
    }
}
