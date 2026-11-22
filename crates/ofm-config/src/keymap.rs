//! Keymaps: presets + user overrides, lookup by context, conflict detection.
//!
//! File format (ADR-0005): top-level `version`, `preset`, `typing`; sections `[global] [panels]
//! [cmdline] [viewer] [dialog]` with `"<chord>" = "<command id>"` entries. A user `keymap.toml`
//! has the same shape and is merged over the selected preset: same chord in the same section
//! replaces the preset binding; `"<chord>" = ""` unbinds it. The preset itself is selected by
//! `config.toml` (`[keymap] preset`); a `preset` key inside `keymap.toml` is ignored.
//!
//! Resolution rule (T8.8): entries apply in **file order** (parsed with `toml_edit`, which
//! preserves definition order); when one chord is bound twice in a section, the **last**
//! definition wins and the clash is reported as [`KeymapWarning::DuplicateChord`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ofm_core::{CommandId, KeyChord, KeyContext, commands};

use crate::defaults;
use crate::error::{ConfigError, ConfigWarning, Warning};
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

/// A non-fatal problem found while loading a keymap, typed so the UI can show it (F29).
/// The corresponding entry is skipped or resolved last-wins; loading always continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapWarning {
    /// The same chord was bound to different commands more than once in one section; the last
    /// definition in file order wins.
    DuplicateChord {
        /// Context section (`global`, `panels`, ...).
        section: String,
        /// The chord (normalised).
        chord: KeyChord,
        /// The commands that claimed it, in file order.
        commands: Vec<CommandId>,
    },
    /// A chord is bound in `[global]` but a different command claims the same chord in a more
    /// specific section, hiding the global binding there.
    ShadowedGlobal {
        /// The chord.
        chord: KeyChord,
        /// Command bound in `[global]`.
        global_command: CommandId,
        /// Section that shadows it.
        section: String,
        /// Command that wins in `section`.
        command: CommandId,
    },
    /// Chord text that does not parse; the entry is skipped.
    UnparsableChord {
        /// Context section.
        section: String,
        /// The chord text as written.
        chord: String,
        /// Parser message.
        message: String,
    },
    /// Command id that is not in the registry; the entry is skipped.
    UnknownCommand {
        /// Context section.
        section: String,
        /// The chord.
        chord: String,
        /// The unknown command id.
        command: String,
    },
    /// The command value is not a string; the entry is skipped.
    CommandNotString {
        /// Context section.
        section: String,
        /// The chord.
        chord: String,
    },
    /// A known section whose value is not a table; the section is skipped.
    SectionNotTable {
        /// Section name.
        section: String,
    },
    /// A `typing` value that is not recognised; ignored.
    UnknownTypingMode {
        /// The value as written.
        value: String,
    },
}

impl KeymapWarning {
    /// Human-readable message (matches the legacy [`Warning`] text).
    pub fn message(&self) -> String {
        match self {
            KeymapWarning::DuplicateChord {
                section,
                chord,
                commands,
            } => {
                let names: Vec<_> = commands.iter().map(|c| c.as_str()).collect();
                format!(
                    "key {chord} is bound more than once in [{section}]: {} (last wins)",
                    names.join(", ")
                )
            }
            KeymapWarning::ShadowedGlobal {
                chord,
                global_command,
                section,
                command,
            } => format!(
                "key {chord} is bound in [global] as {global_command} but shadowed in [{section}] as {command}"
            ),
            KeymapWarning::UnparsableChord {
                section,
                chord,
                message,
            } => format!("[{section}] {chord}: {message}"),
            KeymapWarning::UnknownCommand {
                section,
                chord,
                command,
            } => format!("[{section}] {chord}: unknown command {command:?}"),
            KeymapWarning::CommandNotString { section, chord } => {
                format!("[{section}] {chord}: command must be a string")
            }
            KeymapWarning::SectionNotTable { section } => {
                format!("[{section}] is not a table")
            }
            KeymapWarning::UnknownTypingMode { value } => {
                format!("unknown typing mode {value:?}")
            }
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

/// Top-level keys of a keymap file that are part of the schema (ADR-0005). `preset` is accepted
/// and ignored by design; anything else unknown is reported as [`ConfigWarning::UnknownKey`].
const KNOWN_TOP_LEVEL: &[&str] = &["version", "preset", "typing"];

/// Parse a keymap file with `toml_edit` so entries keep their definition order (last wins is
/// then literally the last line of the file). Unknown command ids, unparsable chords and bad
/// shapes are collected as warnings and skipped; syntax errors are returned as `Err`.
fn parse_file(
    text: &str,
    origin: &str,
) -> Result<(Parsed, Vec<KeymapWarning>, Vec<ConfigWarning>), ConfigError> {
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| ConfigError::Parse {
            path: PathBuf::from(origin),
            message: e.to_string(),
        })?;
    let mut keymap_warnings = Vec::new();
    let mut config_warnings = Vec::new();
    let mut typing = None;
    let mut entries = Vec::new();
    for (key, item) in doc.iter() {
        if KNOWN_TOP_LEVEL.contains(&key) {
            if key == "typing" {
                match item.as_str() {
                    None | Some("") => {}
                    Some(t) => match TypingMode::from_name(t) {
                        Some(m) => typing = Some(m),
                        None => {
                            keymap_warnings.push(KeymapWarning::UnknownTypingMode {
                                value: t.to_owned(),
                            });
                        }
                    },
                }
            }
            continue;
        }
        let Some(ctx) = KeyContext::from_section(key) else {
            config_warnings.push(ConfigWarning::UnknownKey {
                file: PathBuf::from(origin),
                path: key.to_owned(),
            });
            continue;
        };
        let Some(table) = item.as_table() else {
            keymap_warnings.push(KeymapWarning::SectionNotTable {
                section: key.to_owned(),
            });
            continue;
        };
        for (chord_text, cmd_item) in table.iter() {
            let chord = match chord_text.parse::<KeyChord>() {
                Ok(c) => c,
                Err(e) => {
                    keymap_warnings.push(KeymapWarning::UnparsableChord {
                        section: key.to_owned(),
                        chord: chord_text.to_owned(),
                        message: e.to_string(),
                    });
                    continue;
                }
            };
            let Some(cmd_text) = cmd_item.as_str() else {
                keymap_warnings.push(KeymapWarning::CommandNotString {
                    section: key.to_owned(),
                    chord: chord.to_string(),
                });
                continue;
            };
            let cmd = if cmd_text.is_empty() {
                None
            } else {
                match CommandId::lookup(cmd_text) {
                    Some(c) => Some(c),
                    None => {
                        keymap_warnings.push(KeymapWarning::UnknownCommand {
                            section: key.to_owned(),
                            chord: chord.to_string(),
                            command: cmd_text.to_owned(),
                        });
                        continue;
                    }
                }
            };
            entries.push(Entry { ctx, chord, cmd });
        }
    }
    Ok((Parsed { typing, entries }, keymap_warnings, config_warnings))
}

impl Keymap {
    fn empty() -> Keymap {
        Keymap {
            typing: TypingMode::CommandLine,
            bindings: HashMap::new(),
            duplicates: Vec::new(),
        }
    }

    /// Apply a parsed file on top of `self`. Entries apply in file order, so the last definition
    /// of a chord wins. Chords that appear twice within the file (after normalisation) are
    /// recorded as duplicates.
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

    /// Duplicates recorded by [`apply`](Self::apply), as typed warnings (already sorted).
    fn duplicate_warnings(&self) -> Vec<KeymapWarning> {
        self.duplicates
            .iter()
            .map(|c| KeymapWarning::DuplicateChord {
                section: c.context.section().to_owned(),
                chord: c.chord,
                commands: c.commands.clone(),
            })
            .collect()
    }

    /// `[global]` bindings hidden by a different `[panels]` binding of the same chord (the panels
    /// lookup runs before the global fallback in the main window). Rebinding a global chord in
    /// `cmdline`/`viewer`/`dialog` is normal fallback behaviour and not reported.
    fn shadowed_global_warnings(&self) -> Vec<KeymapWarning> {
        let mut chords: Vec<KeyChord> = self
            .bindings
            .keys()
            .filter(|(ctx, _)| *ctx == KeyContext::Panels)
            .map(|(_, chord)| *chord)
            .collect();
        chords.sort();
        chords
            .into_iter()
            .filter_map(|chord| {
                let command = self.lookup(KeyContext::Panels, chord)?;
                let global_command = self.lookup(KeyContext::Global, chord)?;
                (global_command != command).then(|| KeymapWarning::ShadowedGlobal {
                    chord,
                    global_command,
                    section: KeyContext::Panels.section().to_owned(),
                    command,
                })
            })
            .collect()
    }

    /// Parse a preset or user keymap file; unknown command ids and bad chords are collected as
    /// warnings and skipped. Syntax errors are returned as `Err`.
    pub fn from_toml(text: &str, origin: &str) -> Result<(Keymap, Vec<Warning>), ConfigError> {
        let (km, keymap_warnings, config_warnings) = Keymap::from_toml_with_warnings(text, origin)?;
        let file = PathBuf::from(origin);
        let mut warnings: Vec<Warning> = keymap_warnings
            .iter()
            .map(|w| Warning {
                file: Some(file.clone()),
                message: w.message(),
            })
            .collect();
        warnings.extend(config_warnings.iter().map(ConfigWarning::to_warning));
        Ok((km, warnings))
    }

    /// Typed form of [`from_toml`](Self::from_toml): returns keymap-level and file-level
    /// warnings separately so the UI can present them (F29).
    pub fn from_toml_with_warnings(
        text: &str,
        origin: &str,
    ) -> Result<(Keymap, Vec<KeymapWarning>, Vec<ConfigWarning>), ConfigError> {
        let (parsed, mut keymap_warnings, config_warnings) = parse_file(text, origin)?;
        let mut km = Keymap::empty();
        km.apply(parsed);
        keymap_warnings.extend(km.duplicate_warnings());
        keymap_warnings.extend(km.shadowed_global_warnings());
        Ok((km, keymap_warnings, config_warnings))
    }

    /// Built-in preset (always valid; a test enforces it).
    pub fn preset(preset: Preset) -> Keymap {
        let (origin, text) = preset.source();
        Keymap::from_toml(text, origin)
            .map(|(k, _)| k)
            .unwrap_or_else(|_| Keymap::empty())
    }

    /// Preset + user `keymap.toml` + `typing` override from config. A broken user file yields
    /// the plain preset and a warning.
    pub fn load(
        paths: &ConfigPaths,
        preset: Preset,
        typing: Option<TypingMode>,
    ) -> (Keymap, Vec<Warning>) {
        let (km, keymap_warnings, config_warnings) =
            Keymap::load_with_warnings(paths, preset, typing);
        let file = paths.keymap_file();
        let mut warnings: Vec<Warning> = keymap_warnings
            .iter()
            .map(|w| Warning {
                file: Some(file.clone()),
                message: w.message(),
            })
            .collect();
        warnings.extend(config_warnings.iter().map(ConfigWarning::to_warning));
        (km, warnings)
    }

    /// Typed form of [`load`](Self::load): keymap-level warnings (duplicates, shadowed globals,
    /// bad entries) and file-level warnings (unknown sections, unreadable/invalid file) so the
    /// UI can show them (F29). Presets are conflict-free (tested), so every duplicate/shadow
    /// reported here comes from the user file.
    pub fn load_with_warnings(
        paths: &ConfigPaths,
        preset: Preset,
        typing: Option<TypingMode>,
    ) -> (Keymap, Vec<KeymapWarning>, Vec<ConfigWarning>) {
        let mut km = Keymap::preset(preset);
        let mut keymap_warnings = Vec::new();
        let mut config_warnings = Vec::new();
        let file = paths.keymap_file();
        let origin = file.to_string_lossy().into_owned();
        match io::read_optional(&file) {
            Ok(None) => {}
            Err(e) => config_warnings.push(ConfigWarning::Unreadable {
                file: file.clone(),
                message: e.to_string(),
            }),
            Ok(Some(text)) => match parse_file(&text, &origin) {
                Err(e) => config_warnings.push(ConfigWarning::Invalid {
                    file: file.clone(),
                    message: format!("{e}; using the {preset:?} preset"),
                }),
                Ok((parsed, mut kw, mut cw)) => {
                    let mut merged = km.clone();
                    merged.apply(parsed);
                    keymap_warnings.append(&mut kw);
                    config_warnings.append(&mut cw);
                    keymap_warnings.extend(merged.duplicate_warnings());
                    keymap_warnings.extend(merged.shadowed_global_warnings());
                    km = merged;
                }
            },
        }
        if let Some(t) = typing {
            km.typing = t;
        }
        (km, keymap_warnings, config_warnings)
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

    /// T8.12: laptops without a numpad must reach every mask/all/none selection command
    /// with at least one chord that is not a keypad key (Gray*/KP*).
    #[test]
    fn mask_selection_commands_have_non_numpad_chords_in_every_preset() {
        use ofm_core::keychord::Key;
        let selection = [
            cmd::SELECT_BY_MASK,
            cmd::SELECT_UNMASK,
            cmd::SELECT_INVERT,
            cmd::SELECT_ALL,
            cmd::SELECT_NONE,
        ];
        let is_numpad = |k: Key| {
            matches!(
                k,
                Key::Keypad(_) | Key::GrayPlus | Key::GrayMinus | Key::GrayStar | Key::GraySlash
            )
        };
        for (name, p) in [("far", Preset::Far), ("tc", Preset::Tc), ("mc", Preset::Mc)] {
            let km = Keymap::preset(p);
            for c in selection {
                let non_numpad = km
                    .chords_for(c)
                    .iter()
                    .any(|(ctx, ch)| *ctx == KeyContext::Panels && !is_numpad(ch.key));
                assert!(non_numpad, "{name}: {c:?} has no non-numpad chord");
            }
        }
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
        let (user, kw, cw) = parse_file(
            "typing = \"quick_search\"\n[global]\n\"F10\" = \"app.menu\"\n\"F1\" = \"\"\n",
            "u",
        )
        .unwrap();
        assert!(kw.is_empty() && cw.is_empty());
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
        assert_eq!(w.len(), 4, "{w:?}");
        // The typo'd section is a typed ConfigWarning::UnknownKey, not silently ignored.
        assert!(
            w.iter().any(|w| w.message.contains("\"nonsense\"")),
            "{w:?}"
        );
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
        // Last definition in file order wins: "Ctrl+J" (app.help) is after "ctrl+j".
        assert_eq!(
            km.lookup(KeyContext::Global, chord("Ctrl+J")),
            Some(cmd::APP_HELP)
        );
    }

    #[test]
    fn typed_warnings_cover_duplicates_shadowing_and_unknown_keys() {
        let (km, kw, cw) = Keymap::from_toml_with_warnings(
            "typing = \"bogus\"\n[global]\n\"ctrl+j\" = \"app.jobs\"\n\"Ctrl+J\" = \"app.help\"\n\"F3\" = \"app.menu\"\n[panels]\n\"F3\" = \"panel.home\"\n[panles]\nx = 1\n",
            "user-keymap.toml",
        )
        .unwrap();
        assert!(km.lookup(KeyContext::Global, chord("Ctrl+J")) == Some(cmd::APP_HELP));
        assert!(
            kw.iter().any(|w| matches!(
                w,
                KeymapWarning::DuplicateChord { section, chord, commands }
                    if section.as_str() == "global" && *chord == "Ctrl+J".parse().unwrap()
                        && commands == &vec![cmd::APP_JOBS, cmd::APP_HELP]
            )),
            "{kw:?}"
        );
        assert!(
            kw.iter().any(|w| matches!(
                w,
                KeymapWarning::ShadowedGlobal { chord, section, .. }
                    if *chord == "F3".parse().unwrap() && section.as_str() == "panels"
            )),
            "{kw:?}"
        );
        assert!(
            kw.iter()
                .any(|w| matches!(w, KeymapWarning::UnknownTypingMode { value } if value.as_str() == "bogus")),
            "{kw:?}"
        );
        assert_eq!(
            cw,
            vec![ConfigWarning::UnknownKey {
                file: PathBuf::from("user-keymap.toml"),
                path: "panles".into(),
            }]
        );
        // The legacy wrapper renders the same information as flat warnings.
        let (_, flat) = Keymap::from_toml(
            "typing = \"bogus\"\n[global]\n\"ctrl+j\" = \"app.jobs\"\n\"Ctrl+J\" = \"app.help\"\n\"F3\" = \"app.menu\"\n[panels]\n\"F3\" = \"panel.home\"\n[panles]\nx = 1\n",
            "user-keymap.toml",
        )
        .unwrap();
        assert_eq!(flat.len(), 4, "{flat:?}");
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
    fn load_with_warnings_is_typed_and_deterministic() {
        let d = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"));
        io::atomic_write(
            &paths.keymap_file(),
            "[global]\n\"ctrl+j\" = \"app.jobs\"\n\"Ctrl+J\" = \"app.help\"\n[pannels]\n\"F2\" = \"app.help\"\n",
        )
        .unwrap();
        let (_, kw, cw) = Keymap::load_with_warnings(&paths, Preset::Far, None);
        assert_eq!(kw.len(), 1, "{kw:?}");
        assert!(
            matches!(&kw[0], KeymapWarning::DuplicateChord { section, chord, commands }
                if section.as_str() == "global" && *chord == "Ctrl+J".parse().unwrap()
                    && commands == &vec![cmd::APP_JOBS, cmd::APP_HELP]),
            "{kw:?}"
        );
        assert_eq!(
            cw,
            vec![ConfigWarning::UnknownKey {
                file: paths.keymap_file(),
                path: "pannels".into(),
            }]
        );
        // File-order resolution: the later line wins regardless of spelling.
        let (km, _, _) = Keymap::load_with_warnings(&paths, Preset::Far, None);
        assert_eq!(
            km.lookup(KeyContext::Global, chord("Ctrl+J")),
            Some(cmd::APP_HELP)
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
