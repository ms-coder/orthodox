//! Key chords. Owned by T1.3 (parser/formatter bodies); the types are frozen.
//!
//! Text syntax (ADR-0005): modifiers `Ctrl+Alt+Shift+Super+` (any order, canonical order on output),
//! then one key name: single printable character (letters are case-insensitive, stored upper-case),
//! `F1`..`F24`, `Up Down Left Right PgUp PgDn Home End Enter Esc Tab Space Backspace Del Ins Menu`,
//! keypad `Gray+ Gray- Gray* Gray/` (alias `Gray\u{2212}` for minus), `KP0`..`KP9`, `KP.`.
//! Examples: `Ctrl+Shift+F5`, `Alt+=`, `Ctrl+\`, `Ctrl+,`, `Gray+`.

use std::fmt;
use std::str::FromStr;

/// A physical/logical key without modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// Printable character, upper-cased for letters (`'A'`, `'='`, `'\\'`).
    Char(char),
    /// Function key 1..=24.
    F(u8),
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Enter / Return (keypad Enter maps here too).
    Enter,
    /// Escape.
    Escape,
    /// Tab.
    Tab,
    /// Space bar.
    Space,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// Context-menu key.
    Menu,
    /// Keypad plus ("Gray+").
    GrayPlus,
    /// Keypad minus ("Gray-").
    GrayMinus,
    /// Keypad multiply ("Gray*").
    GrayStar,
    /// Keypad divide ("Gray/").
    GraySlash,
    /// Keypad digit or decimal point (`'0'..='9'`, `'.'`).
    Keypad(char),
}

/// Modifier set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Mods {
    /// Control.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Super / Meta.
    pub super_: bool,
}

/// A key plus modifiers; the unit the keymap maps to a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    /// The key.
    pub key: Key,
    /// Modifiers.
    pub mods: Mods,
}

/// Parse failure for [`KeyChord::from_str`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid key chord {input:?}: {reason}")]
pub struct KeyChordParseError {
    /// The offending text.
    pub input: String,
    /// Why.
    pub reason: String,
}

impl FromStr for KeyChord {
    type Err = KeyChordParseError;
    /// Parse the syntax in the module docs. T1.3.
    fn from_str(_s: &str) -> Result<Self, Self::Err> {
        todo!("T1.3: keychord parser")
    }
}

impl fmt::Display for KeyChord {
    /// Canonical text form; `parse(display(x)) == x`. T1.3.
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("T1.3: keychord formatter")
    }
}
