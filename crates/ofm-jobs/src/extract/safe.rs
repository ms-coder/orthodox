//! Second line of defence for extraction (ADR-0008 "Zip-slip policy"): the index already flags
//! unsafe entries, but a job that writes to the user's disk does not trust flags. Every output
//! path and every link target is validated again here, from the raw bytes, with pure functions
//! that never touch the file system.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::plan::NAME_MAX;

/// Why a path or link target was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The path is empty.
    Empty,
    /// The path starts with `/`.
    Absolute,
    /// A component is `..` (paths) or a `..` that leaves the extraction tree / follows a name
    /// (link targets).
    Traversal,
    /// A component is empty (`a//b`, trailing `/`) or `.`.
    OddComponent,
    /// The name contains a NUL byte.
    Nul,
    /// A component is longer than the file system allows.
    TooLong,
}

impl Refusal {
    /// Short English description for the report.
    pub(super) fn describe(self) -> &'static str {
        match self {
            Refusal::Empty => "empty path",
            Refusal::Absolute => "absolute path",
            Refusal::Traversal => "path leaves the destination",
            Refusal::OddComponent => "empty or '.' path component",
            Refusal::Nul => "NUL byte in path",
            Refusal::TooLong => "path component too long",
        }
    }
}

/// Validate an entry path (after the index normalised it): relative, plain components only.
/// Returns the components. Works on raw bytes, so `Path` normalisation cannot hide anything.
pub(super) fn validate_entry_path(path: &Path) -> Result<Vec<OsString>, Refusal> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() {
        return Err(Refusal::Empty);
    }
    if bytes.contains(&0) {
        return Err(Refusal::Nul);
    }
    if bytes[0] == b'/' {
        return Err(Refusal::Absolute);
    }
    let mut out = Vec::new();
    for comp in bytes.split(|b| *b == b'/') {
        match comp {
            b"" | b"." => return Err(Refusal::OddComponent),
            b".." => return Err(Refusal::Traversal),
            c if c.len() > NAME_MAX => return Err(Refusal::TooLong),
            c => out.push(OsString::from_vec(c.to_vec())),
        }
    }
    Ok(out)
}

/// Whether a symlink with text `target`, created at relative output path `rel` (below the
/// destination directory), is acceptable. Rules (stricter than the index's lexical check, because
/// the link is created on the real disk):
/// - relative, non-empty, no NUL;
/// - `..` only as a leading run (never after a name: `a/../b` is refused), and the run may not
///   climb above the destination directory. Without a `..` behind a name no later link can
///   traverse "through" a symlink and leave the tree (a link to an ancestor followed by `../..`
///   in another link would otherwise escape: the lexical and the physical parent differ).
pub(super) fn link_target_ok(rel: &Path, target: &Path) -> Result<(), Refusal> {
    let bytes = target.as_os_str().as_bytes();
    if bytes.is_empty() {
        return Err(Refusal::Empty);
    }
    if bytes.contains(&0) {
        return Err(Refusal::Nul);
    }
    if bytes[0] == b'/' {
        return Err(Refusal::Absolute);
    }
    let mut depth = rel.components().count().saturating_sub(1);
    let mut seen_name = false;
    for comp in bytes.split(|b| *b == b'/') {
        match comp {
            b"" | b"." => {}
            b".." if seen_name => return Err(Refusal::Traversal),
            b".." => {
                depth = depth.checked_sub(1).ok_or(Refusal::Traversal)?;
            }
            _ => seen_name = true,
        }
    }
    Ok(())
}

/// `path` relative to the parent of the selected item `sel` (each selected item lands directly in
/// the destination: `a/b/c` with selection `a/b` becomes `b/c`; the empty selection keeps every
/// path). `None` if `path` is not below `sel`.
pub(super) fn relative_to_selection(path: &Path, sel: &Path) -> Option<PathBuf> {
    if !path.starts_with(sel) {
        return None;
    }
    let base = sel.parent().unwrap_or(Path::new(""));
    path.strip_prefix(base).ok().map(Path::to_path_buf)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn v(p: &str) -> Result<Vec<String>, Refusal> {
        validate_entry_path(Path::new(p))
            .map(|c| c.iter().map(|s| s.to_string_lossy().into_owned()).collect())
    }

    #[test]
    fn plain_paths_pass_and_hostile_ones_do_not() {
        assert_eq!(v("a/b/c"), Ok(vec!["a".into(), "b".into(), "c".into()]));
        assert_eq!(v("-rf"), Ok(vec!["-rf".into()]));
        assert_eq!(v("a\nb/x"), Ok(vec!["a\nb".into(), "x".into()]));
        assert_eq!(v(""), Err(Refusal::Empty));
        assert_eq!(v("/etc/passwd"), Err(Refusal::Absolute));
        assert_eq!(v("../x"), Err(Refusal::Traversal));
        assert_eq!(v("a/../b"), Err(Refusal::Traversal));
        assert_eq!(v("a/.."), Err(Refusal::Traversal));
        assert_eq!(v("a//b"), Err(Refusal::OddComponent));
        assert_eq!(v("a/b/"), Err(Refusal::OddComponent));
        assert_eq!(v("./a"), Err(Refusal::OddComponent));
        assert_eq!(v("a/./b"), Err(Refusal::OddComponent));
        assert_eq!(v(&"x".repeat(256)), Err(Refusal::TooLong));
        let nul = OsString::from_vec(b"a\0b".to_vec());
        assert_eq!(validate_entry_path(Path::new(&nul)), Err(Refusal::Nul));
        let raw = OsString::from_vec(vec![b'a', 0xff, 0xfe]);
        assert!(
            validate_entry_path(Path::new(&raw)).is_ok(),
            "non-UTF-8 is fine"
        );
    }

    #[test]
    fn link_targets() {
        let ok = |rel: &str, t: &str| link_target_ok(Path::new(rel), Path::new(t));
        assert_eq!(ok("l", "file"), Ok(()));
        assert_eq!(ok("d/l", "../file"), Ok(()));
        assert_eq!(ok("d/e/l", "../../x/y"), Ok(()));
        assert_eq!(ok("l", "../x"), Err(Refusal::Traversal), "above the root");
        assert_eq!(ok("d/l", "../../x"), Err(Refusal::Traversal));
        assert_eq!(ok("l", "/etc"), Err(Refusal::Absolute));
        assert_eq!(ok("l", ""), Err(Refusal::Empty));
        assert_eq!(
            ok("l", "a/../b"),
            Err(Refusal::Traversal),
            "no .. after a name"
        );
        assert_eq!(ok("d/l", "s/.."), Err(Refusal::Traversal));
        assert_eq!(ok("l", "./a"), Ok(()));
    }

    #[test]
    fn selection_base() {
        let r = |p: &str, s: &str| relative_to_selection(Path::new(p), Path::new(s));
        assert_eq!(r("a/b/c", "a/b"), Some("b/c".into()));
        assert_eq!(r("a/b", "a/b"), Some("b".into()));
        assert_eq!(r("a/b/c", ""), Some("a/b/c".into()));
        assert_eq!(r("a/bc", "a/b"), None, "component-wise prefix");
        assert_eq!(r("x", "a"), None);
    }
}
