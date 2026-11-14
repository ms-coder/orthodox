//! GDK key event -> [`KeyChord`] conversion (the only place GDK key codes are interpreted).
//!
//! Rules: letters are upper-cased; keypad keys map to `Gray+ Gray- Gray* Gray/`/`KPn`; `Shift+Tab`
//! (`ISO_Left_Tab`) becomes `Shift+Tab`. When Shift is held or the active layout produced a
//! non-ASCII character (e.g. Cyrillic), the key is re-translated from the hardware keycode with
//! Shift cleared on layout group 0, so `Shift+=` is `Shift+=` (never an unparseable `+`) and
//! Alt/Ctrl shortcuts work on non-Latin layouts. (M1 review finding "Key::Char('+')".)

use gtk::gdk;
use gtk::gdk::prelude::DisplayExtManual;
use gtk::glib::translate::IntoGlib;
use ofm_core::{Key, KeyChord, Mods};

fn mods_from(state: gdk::ModifierType) -> Mods {
    Mods {
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        super_: state.contains(gdk::ModifierType::SUPER_MASK),
    }
}

/// Pure mapping of a GDK key value plus modifier state to a chord (no display needed).
/// `None` for modifier keys alone and for keys without a [`Key`] equivalent.
pub fn chord_from_key(key: gdk::Key, state: gdk::ModifierType) -> Option<KeyChord> {
    let mut mods = mods_from(state);
    let raw: u32 = key.into_glib();
    let mapped = match key {
        gdk::Key::Up | gdk::Key::KP_Up => Key::Up,
        gdk::Key::Down | gdk::Key::KP_Down => Key::Down,
        gdk::Key::Left | gdk::Key::KP_Left => Key::Left,
        gdk::Key::Right | gdk::Key::KP_Right => Key::Right,
        gdk::Key::Page_Up | gdk::Key::KP_Page_Up => Key::PageUp,
        gdk::Key::Page_Down | gdk::Key::KP_Page_Down => Key::PageDown,
        gdk::Key::Home | gdk::Key::KP_Home => Key::Home,
        gdk::Key::End | gdk::Key::KP_End => Key::End,
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => Key::Enter,
        gdk::Key::Escape => Key::Escape,
        gdk::Key::Tab => Key::Tab,
        gdk::Key::ISO_Left_Tab => {
            mods.shift = true;
            Key::Tab
        }
        gdk::Key::space => Key::Space,
        gdk::Key::BackSpace => Key::Backspace,
        gdk::Key::Delete | gdk::Key::KP_Delete => Key::Delete,
        gdk::Key::Insert | gdk::Key::KP_Insert => Key::Insert,
        gdk::Key::Menu => Key::Menu,
        gdk::Key::KP_Add => Key::GrayPlus,
        gdk::Key::KP_Subtract => Key::GrayMinus,
        gdk::Key::KP_Multiply => Key::GrayStar,
        gdk::Key::KP_Divide => Key::GraySlash,
        gdk::Key::KP_Decimal => Key::Keypad('.'),
        _ => {
            // F1..F24 are contiguous from 0xffbe.
            if (0xffbe..=0xffd5).contains(&raw) {
                Key::F((raw - 0xffbd) as u8)
            } else if (0xffb0..=0xffb9).contains(&raw) {
                // KP_0..KP_9
                Key::Keypad(char::from(b'0' + (raw - 0xffb0) as u8))
            } else {
                let c = key.to_unicode()?;
                if c.is_control() {
                    return None;
                }
                Key::Char(c.to_uppercase().next().unwrap_or(c))
            }
        }
    };
    Some(KeyChord { key: mapped, mods })
}

/// Chord for a GTK key-pressed event. Applies the Shift/non-Latin re-translation described in the
/// module docs (needs a default display; falls back to the pure mapping without one).
pub fn chord_from_event(
    keyval: gdk::Key,
    keycode: u32,
    state: gdk::ModifierType,
) -> Option<KeyChord> {
    let chord = chord_from_key(keyval, state)?;
    let Key::Char(c) = chord.key else {
        return Some(chord);
    };
    let needs_base = chord.mods.shift || !c.is_ascii();
    if !needs_base {
        return Some(chord);
    }
    let base = gdk::Display::default()
        .and_then(|d| d.translate_key(keycode, state & !gdk::ModifierType::SHIFT_MASK, 0))
        .and_then(|t| chord_from_key(t.0, state));
    match base {
        Some(KeyChord {
            key: Key::Char(b), ..
        }) => Some(KeyChord {
            key: Key::Char(b),
            mods: chord.mods,
        }),
        _ => Some(chord),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const NONE: gdk::ModifierType = gdk::ModifierType::empty();

    fn parse(s: &str) -> KeyChord {
        s.parse().expect("valid chord in test")
    }

    #[test]
    fn named_keys() {
        assert_eq!(chord_from_key(gdk::Key::Up, NONE), Some(parse("Up")));
        assert_eq!(chord_from_key(gdk::Key::Tab, NONE), Some(parse("Tab")));
        assert_eq!(
            chord_from_key(gdk::Key::ISO_Left_Tab, gdk::ModifierType::SHIFT_MASK),
            Some(parse("Shift+Tab"))
        );
        assert_eq!(chord_from_key(gdk::Key::F5, NONE), Some(parse("F5")));
        assert_eq!(chord_from_key(gdk::Key::F12, NONE), Some(parse("F12")));
        assert_eq!(chord_from_key(gdk::Key::KP_Add, NONE), Some(parse("Gray+")));
        assert_eq!(
            chord_from_key(gdk::Key::KP_Subtract, NONE),
            Some(parse("Gray-"))
        );
        assert_eq!(
            chord_from_key(gdk::Key::KP_Multiply, NONE),
            Some(parse("Gray*"))
        );
        assert_eq!(chord_from_key(gdk::Key::KP_5, NONE), Some(parse("KP5")));
        assert_eq!(chord_from_key(gdk::Key::Return, NONE), Some(parse("Enter")));
        assert_eq!(chord_from_key(gdk::Key::Shift_L, NONE), None);
    }

    #[test]
    fn letters_and_mods() {
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        assert_eq!(chord_from_key(gdk::Key::p, ctrl), Some(parse("Ctrl+P")));
        assert_eq!(
            chord_from_key(gdk::Key::P, ctrl | gdk::ModifierType::SHIFT_MASK),
            Some(parse("Ctrl+Shift+P"))
        );
        assert_eq!(
            chord_from_key(gdk::Key::equal, gdk::ModifierType::ALT_MASK),
            Some(parse("Alt+="))
        );
    }
}
