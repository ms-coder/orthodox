//! Path completion over already-listed names (F11): pure — the caller (command line module)
//! supplies the candidate names of the relevant directory, no file system access happens here.

use std::ffi::{OsStr, OsString};

use super::os_from_bytes;

/// Result of [`complete_in`] on a candidate set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completion {
    /// Candidates that start with the prefix, in the order they were given.
    pub matches: Vec<OsString>,
    /// The longest prefix shared by **all** matches beyond the typed prefix: what one completion
    /// step appends (empty when there is nothing to add or nothing matches).
    pub extend: OsString,
    /// Exactly one candidate: the caller may finish the word (append `/` for a directory).
    pub unique: bool,
}

/// Complete `prefix` against `names`: byte-exact prefix matching, so non-UTF-8 names complete
/// like any other. `names` is not re-sorted (the panel listing order is meaningful); the caller
/// shows [`Completion::matches`] in a list when `extend` does not get the user any further.
pub fn complete_in(names: &[OsString], prefix: &OsStr) -> Completion {
    let prefix_bytes = super::bytes_of(prefix);
    let matches: Vec<OsString> = names
        .iter()
        .filter(|n| super::bytes_of(n).starts_with(&prefix_bytes))
        .cloned()
        .collect();
    let unique = matches.len() == 1;
    let common = common_prefix(&matches);
    let extend_bytes = common
        .as_encoded_bytes()
        .strip_prefix(&*prefix_bytes)
        .unwrap_or(&[])
        .to_vec();
    Completion {
        extend: os_from_bytes(&extend_bytes),
        unique,
        matches,
    }
}

/// The longest common byte prefix of `names` (empty for an empty set or when the names disagree
/// on the first byte).
pub fn common_prefix(names: &[OsString]) -> OsString {
    let Some(first) = names.first() else {
        return OsString::new();
    };
    let mut len = first.as_encoded_bytes().len();
    for name in &names[1..] {
        let bytes = name.as_encoded_bytes();
        len = len.min(bytes.len());
        let mut i = 0;
        while i < len && first.as_encoded_bytes()[i] == bytes[i] {
            i += 1;
        }
        len = i;
        if len == 0 {
            break;
        }
    }
    os_from_bytes(&first.as_encoded_bytes()[..len])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::ffi::{OsStr, OsString};

    use super::*;

    fn names(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn completes_common_prefix_and_unique() {
        let set = names(&["Makefile", "Makefile.in", "main.c", "foo"]);
        let c = complete_in(&set, OsStr::new("Make"));
        assert_eq!(c.matches, names(&["Makefile", "Makefile.in"]));
        assert_eq!(c.extend, OsString::from("file"));
        assert!(!c.unique);
        let c = complete_in(&set, OsStr::new("main"));
        assert!(c.unique);
        assert_eq!(c.extend, OsString::from(".c"));
        assert_eq!(c.matches, names(&["main.c"]));
    }

    #[test]
    fn no_match_and_empty_prefix() {
        let set = names(&["a", "ab"]);
        let c = complete_in(&set, OsStr::new("z"));
        assert!(c.matches.is_empty());
        assert_eq!(c.extend, OsString::new());
        assert!(!c.unique);
        let c = complete_in(&set, OsStr::new(""));
        assert_eq!(c.matches.len(), 2);
        assert_eq!(c.extend, OsString::from("a")); // common prefix of "a" and "ab"
    }

    #[test]
    fn common_prefix_disagreement() {
        assert_eq!(common_prefix(&names(&["xyz", "abc"])), OsString::new());
        assert_eq!(
            common_prefix(&names(&["a", "ab", "abc"])),
            OsString::from("a")
        );
        assert_eq!(common_prefix(&[]), OsString::new());
        assert_eq!(common_prefix(&names(&["solo"])), OsString::from("solo"));
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_names_complete_byte_exact() {
        use std::os::unix::ffi::OsStringExt;
        let set = vec![
            OsString::from_vec(vec![0xff, b'a']),
            OsString::from_vec(vec![0xff, b'b', 0xfe]),
        ];
        let prefix = OsString::from_vec(vec![0xff]);
        let c = complete_in(&set, &prefix);
        assert_eq!(c.matches.len(), 2);
        assert_eq!(c.extend, OsString::new()); // matches share only the typed prefix
    }
}
