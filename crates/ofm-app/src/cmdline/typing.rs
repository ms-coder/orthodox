//! The `command_line` typing mode's key claim (pure; unit-tested): mirrors
//! `navigation::quicksearch::typed_char`, which owns the same decision for `quick_search` mode.
//! A plain printable key that no command claimed focuses the command line and is typed there
//! (ADR-0005). Space counts as printable here — the command line takes it.

use ofm_config::TypingMode;
use ofm_core::KeyChord;

/// The character a key press starts typing into the command line with, if any.
pub fn typed_char_cmdline(
    typing: TypingMode,
    chord: &KeyChord,
    unicode: Option<char>,
) -> Option<char> {
    if typing != TypingMode::CommandLine {
        return None;
    }
    let m = chord.mods;
    if m.ctrl || m.alt || m.super_ {
        return None;
    }
    let c = unicode?;
    (!c.is_control()).then_some(c)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_core::KeyChord;

    use super::*;

    fn chord(s: &str) -> KeyChord {
        s.parse().unwrap()
    }

    #[test]
    fn command_line_mode_claims_plain_printables() {
        for c in ['a', 'Z', '5'] {
            assert_eq!(
                typed_char_cmdline(
                    TypingMode::CommandLine,
                    &chord(&c.to_lowercase().to_string()),
                    Some(c)
                ),
                Some(c),
                "{c}"
            );
        }
        // Space belongs to the command line (unlike quick search).
        assert_eq!(
            typed_char_cmdline(TypingMode::CommandLine, &chord("Space"), Some(' ')),
            Some(' ')
        );
        // No modifiers.
        assert_eq!(
            typed_char_cmdline(TypingMode::CommandLine, &chord("Alt+A"), Some('a')),
            None
        );
        assert_eq!(
            typed_char_cmdline(TypingMode::CommandLine, &chord("Ctrl+A"), Some('a')),
            None
        );
        // Control characters are not typed.
        assert_eq!(
            typed_char_cmdline(TypingMode::CommandLine, &chord("A"), None),
            None
        );
    }

    #[test]
    fn quick_search_mode_claims_nothing_here() {
        assert_eq!(
            typed_char_cmdline(TypingMode::QuickSearch, &chord("A"), Some('a')),
            None
        );
    }
}
