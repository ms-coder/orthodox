//! Keymaps: presets + user overrides, lookup by context, conflict detection.
//!
//! File format (ADR-0005): top-level `version`, `preset`, `typing`; sections `[global] [panels]
//! [cmdline] [viewer] [dialog]` with `"<chord>" = "<command id>"` entries. A user `keymap.toml`
//! has the same shape and is merged over the selected preset: same chord in the same section
//! replaces the preset binding; `"<chord>" = ""` unbinds it.

use std::collections::HashMap;

use ofm_core::{CommandId, KeyChord, KeyContext};

use crate::error::{ConfigError, Warning};
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

/// What printable keys do in the panels context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingMode {
    /// Far/mc: letters go to the command line, Alt+letter = quick search.
    CommandLine,
    /// TC: letters start quick search.
    QuickSearch,
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
    #[allow(dead_code, reason = "T1.3 fills in")]
    bindings: HashMap<(KeyContext, KeyChord), CommandId>,
}

impl Keymap {
    /// Parse a preset or user keymap file; unknown command ids and bad chords are collected as
    /// warnings and skipped.
    pub fn from_toml(_text: &str, _origin: &str) -> Result<(Keymap, Vec<Warning>), ConfigError> {
        todo!("T1.3")
    }
    /// Built-in preset (always valid; a test enforces it).
    pub fn preset(_preset: Preset) -> Keymap {
        todo!("T1.3")
    }
    /// Preset + user `keymap.toml` + `typing` override from config, with warnings.
    pub fn load(
        _paths: &ConfigPaths,
        _preset: Preset,
        _typing: Option<TypingMode>,
    ) -> (Keymap, Vec<Warning>) {
        todo!("T1.3")
    }
    /// Command bound to `chord` in exactly `ctx` (no fallback).
    pub fn lookup(&self, _ctx: KeyContext, _chord: KeyChord) -> Option<CommandId> {
        todo!("T1.3")
    }
    /// Dispatcher resolution order for the main window: `CmdLine` (if `cmdline_has_text`), `Panels`,
    /// `Global`. Dialog and Viewer windows resolve in their own section then `Global`.
    pub fn resolve_main(&self, _chord: KeyChord, _cmdline_has_text: bool) -> Option<CommandId> {
        todo!("T1.3")
    }
    /// All chords bound to `cmd` (any context), for menus and the F-key bar.
    pub fn chords_for(&self, _cmd: CommandId) -> Vec<(KeyContext, KeyChord)> {
        todo!("T1.3")
    }
    /// Commands that have no binding at all (a test asserts this is empty for the far preset).
    pub fn unbound_commands(&self) -> Vec<CommandId> {
        todo!("T1.3")
    }
    /// Same chord claimed twice within one context, or by `Global` and `Panels` at once.
    pub fn conflicts(&self) -> Vec<Conflict> {
        todo!("T1.3")
    }
}
