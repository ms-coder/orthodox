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

fn perr(input: &str, reason: &str) -> KeyChordParseError {
    KeyChordParseError {
        input: input.to_owned(),
        reason: reason.to_owned(),
    }
}

/// Fixed key names accepted on input (the canonical spelling is produced by `key_name`).
const NAMED: &[(&str, Key)] = &[
    ("Up", Key::Up),
    ("Down", Key::Down),
    ("Left", Key::Left),
    ("Right", Key::Right),
    ("PgUp", Key::PageUp),
    ("PageUp", Key::PageUp),
    ("PgDn", Key::PageDown),
    ("PageDown", Key::PageDown),
    ("Home", Key::Home),
    ("End", Key::End),
    ("Enter", Key::Enter),
    ("Return", Key::Enter),
    ("Esc", Key::Escape),
    ("Escape", Key::Escape),
    ("Tab", Key::Tab),
    ("Space", Key::Space),
    ("Backspace", Key::Backspace),
    ("Del", Key::Delete),
    ("Delete", Key::Delete),
    ("Ins", Key::Insert),
    ("Insert", Key::Insert),
    ("Menu", Key::Menu),
    ("Gray+", Key::GrayPlus),
    ("Gray-", Key::GrayMinus),
    ("Gray\u{2212}", Key::GrayMinus),
    ("Gray*", Key::GrayStar),
    ("Gray/", Key::GraySlash),
];

fn parse_key(name: &str, input: &str) -> Result<Key, KeyChordParseError> {
    if name.is_empty() {
        return Err(perr(input, "missing key"));
    }
    if let Some((_, k)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return Ok(*k);
    }
    let lower = name.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('f')
        && !num.is_empty()
        && num.bytes().all(|b| b.is_ascii_digit())
    {
        return match num.parse::<u8>() {
            Ok(n) if (1..=24).contains(&n) => Ok(Key::F(n)),
            _ => Err(perr(input, "function key must be F1..F24")),
        };
    }
    if let Some(kp) = lower.strip_prefix("kp") {
        let mut it = kp.chars();
        return match (it.next(), it.next()) {
            (Some(c), None) if c.is_ascii_digit() || c == '.' => Ok(Key::Keypad(c)),
            _ => Err(perr(input, "keypad key must be KP0..KP9 or KP.")),
        };
    }
    let mut it = name.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => {
            if c == '+' {
                Err(perr(input, "'+' is not a valid key name (use Gray+)"))
            } else if c.is_whitespace() || c.is_control() {
                Err(perr(input, "key must be printable (use Space, Tab, ...)"))
            } else {
                Ok(Key::Char(upper(c)))
            }
        }
        _ => Err(perr(input, "unknown key name")),
    }
}

/// Upper-case a letter when that yields a single character.
fn upper(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

impl FromStr for KeyChord {
    type Err = KeyChordParseError;
    /// Parse the syntax in the module docs: modifiers are consumed greedily from the left, the
    /// rest is the key name (so `Ctrl+,` and `Alt+=` work, `Ctrl++` does not).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = Mods::default();
        let mut rest = s;
        while let Some(i) = rest.find('+') {
            let (token, tail) = (&rest[..i], &rest[i + 1..]);
            if i == 0 || tail.is_empty() {
                break;
            }
            let slot = match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut mods.ctrl,
                "alt" => &mut mods.alt,
                "shift" => &mut mods.shift,
                "super" | "meta" => &mut mods.super_,
                _ => break,
            };
            if *slot {
                return Err(perr(s, "duplicate modifier"));
            }
            *slot = true;
            rest = tail;
        }
        let key = parse_key(rest, s)?;
        Ok(KeyChord { key, mods })
    }
}

fn key_name(key: Key, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let name = match key {
        Key::Char(c) => return write!(f, "{c}"),
        Key::F(n) => return write!(f, "F{n}"),
        Key::Keypad(c) => return write!(f, "KP{c}"),
        Key::Up => "Up",
        Key::Down => "Down",
        Key::Left => "Left",
        Key::Right => "Right",
        Key::PageUp => "PgUp",
        Key::PageDown => "PgDn",
        Key::Home => "Home",
        Key::End => "End",
        Key::Enter => "Enter",
        Key::Escape => "Esc",
        Key::Tab => "Tab",
        Key::Space => "Space",
        Key::Backspace => "Backspace",
        Key::Delete => "Del",
        Key::Insert => "Ins",
        Key::Menu => "Menu",
        Key::GrayPlus => "Gray+",
        Key::GrayMinus => "Gray-",
        Key::GrayStar => "Gray*",
        Key::GraySlash => "Gray/",
    };
    f.write_str(name)
}

impl fmt::Display for KeyChord {
    /// Canonical text form; `parse(display(x)) == x`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.mods.ctrl, "Ctrl+"),
            (self.mods.alt, "Alt+"),
            (self.mods.shift, "Shift+"),
            (self.mods.super_, "Super+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        key_name(self.key, f)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn p(s: &str) -> KeyChord {
        s.parse().unwrap()
    }

    #[test]
    fn parses_examples() {
        assert_eq!(
            p("Ctrl+Shift+F5"),
            KeyChord {
                key: Key::F(5),
                mods: Mods {
                    ctrl: true,
                    shift: true,
                    ..Mods::default()
                }
            }
        );
        assert_eq!(p("Alt+=").key, Key::Char('='));
        assert_eq!(p("Ctrl+\\").key, Key::Char('\\'));
        assert_eq!(p("Ctrl+,").key, Key::Char(','));
        assert_eq!(p("Gray+").key, Key::GrayPlus);
        assert_eq!(p("Gray\u{2212}").key, Key::GrayMinus);
        assert_eq!(p("Shift+Gray*").key, Key::GrayStar);
        assert_eq!(p("KP7").key, Key::Keypad('7'));
        assert_eq!(p("KP.").key, Key::Keypad('.'));
        assert_eq!(p("F24").key, Key::F(24));
        assert_eq!(p("ctrl+a"), p("Ctrl+A"));
        assert_eq!(p("Alt+Ctrl+X"), p("Ctrl+Alt+X"));
        assert_eq!(p("Ctrl+Gray+").key, Key::GrayPlus);
        assert_eq!(p("Ctrl+-").key, Key::Char('-'));
        assert_eq!(p("F").key, Key::Char('F'));
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "Ctrl+",
            "Ctrl++",
            "+",
            "Ctrl+Ctrl+A",
            "F0",
            "F25",
            "Foo",
            "KPx",
            "Ctrl+ ",
            "AB",
            "Ctrl+Shift",
            "Hyper+A",
        ] {
            let r = bad.parse::<KeyChord>();
            assert!(r.is_err(), "{bad:?} should fail, got {r:?}");
        }
    }

    #[test]
    fn display_is_canonical() {
        assert_eq!(
            p("Super+Shift+Alt+Ctrl+Del").to_string(),
            "Ctrl+Alt+Shift+Super+Del"
        );
        for s in [
            "F10",
            "Ctrl+,",
            "Alt+=",
            "Gray+",
            "Gray-",
            "Gray*",
            "Gray/",
            "KP0",
            "KP.",
            "PgUp",
            "Shift+Ins",
            "Ctrl+\\",
            "Esc",
            "Menu",
            "Space",
            "Ctrl+Shift+F24",
        ] {
            assert_eq!(p(s).to_string(), s);
        }
    }

    #[test]
    fn roundtrip_all_keys() {
        let mut keys = vec![
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::PageUp,
            Key::PageDown,
            Key::Home,
            Key::End,
            Key::Enter,
            Key::Escape,
            Key::Tab,
            Key::Space,
            Key::Backspace,
            Key::Delete,
            Key::Insert,
            Key::Menu,
            Key::GrayPlus,
            Key::GrayMinus,
            Key::GrayStar,
            Key::GraySlash,
        ];
        keys.extend((1..=24).map(Key::F));
        keys.extend("0123456789.".chars().map(Key::Keypad));
        keys.extend("AZ09=-,./\\;'[]`\u{c4}\u{416}".chars().map(Key::Char));
        for key in keys {
            for bits in 0..16u8 {
                let mods = Mods {
                    ctrl: bits & 1 != 0,
                    alt: bits & 2 != 0,
                    shift: bits & 4 != 0,
                    super_: bits & 8 != 0,
                };
                let c = KeyChord { key, mods };
                assert_eq!(c.to_string().parse::<KeyChord>().unwrap(), c, "{c}");
            }
        }
    }
}
