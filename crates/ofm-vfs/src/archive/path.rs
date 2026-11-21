//! Extraction-safe path normalization: the zip-slip contract (ADR-0008).
//!
//! Pure functions on byte paths. Backends feed every stored path and link target through here
//! (via `IndexBuilder::push`), so no unsafe path can reach the index tree or an extractor.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use super::error::UnsafeReason;

/// Normalize a stored entry path to a relative path of plain components.
///
/// - `.` components and empty components are dropped; a trailing `/` (directory entries) vanishes;
/// - an absolute path is `AbsolutePath`; any `..` component is `ParentTraversal` (even when it
///   would stay inside the archive: `a/../b` is rejected, strictness beats cleverness);
/// - a NUL byte is `InvalidName`;
/// - backslashes are ordinary name characters on Unix (backends of zip files made on Windows
///   convert `\` to `/` *before* calling this, based on the "made by" host);
/// - the result may be empty: that is the archive root (`./` entries in tar), which callers skip.
pub fn normalize_entry_path(raw: &Path) -> Result<PathBuf, UnsafeReason> {
    if raw.as_os_str().as_bytes().contains(&0) {
        return Err(UnsafeReason::InvalidName);
    }
    let mut out = PathBuf::new();
    for c in raw.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return Err(UnsafeReason::AbsolutePath),
            Component::ParentDir => return Err(UnsafeReason::ParentTraversal),
        }
    }
    Ok(out)
}

/// Resolve `target` of a link stored at `link_path` lexically inside the archive: relative targets
/// are interpreted against the link's parent directory. Returns the normalized path of what the
/// link points at, or `LinkEscape` if the target is absolute, empty, contains NUL, or climbs above
/// the archive root. For hardlinks pass `link_path = ""` (their targets are archive-root relative).
pub fn resolve_link_target(link_path: &Path, target: &Path) -> Result<PathBuf, UnsafeReason> {
    if target.as_os_str().is_empty() || target.as_os_str().as_bytes().contains(&0) {
        return Err(UnsafeReason::LinkEscape);
    }
    let mut parts: Vec<&OsStr> = link_path
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    Component::Normal(n) => Some(n),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    for c in target.components() {
        match c {
            Component::Normal(n) => parts.push(n),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(UnsafeReason::LinkEscape);
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(UnsafeReason::LinkEscape),
        }
    }
    Ok(parts.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> Result<PathBuf, UnsafeReason> {
        normalize_entry_path(Path::new(s))
    }

    #[test]
    fn normalizes_plain_paths() {
        assert_eq!(norm("a/b/c.txt"), Ok("a/b/c.txt".into()));
        assert_eq!(norm("./a//b/"), Ok("a/b".into()));
        assert_eq!(norm("./"), Ok(PathBuf::new()));
        assert_eq!(norm(""), Ok(PathBuf::new()));
        assert_eq!(norm("a\\b"), Ok("a\\b".into()), "backslash is a name char");
    }

    #[test]
    fn rejects_zip_slip_forms() {
        assert_eq!(norm("/etc/passwd"), Err(UnsafeReason::AbsolutePath));
        assert_eq!(norm("../evil"), Err(UnsafeReason::ParentTraversal));
        assert_eq!(norm("a/../../evil"), Err(UnsafeReason::ParentTraversal));
        assert_eq!(norm("a/../b"), Err(UnsafeReason::ParentTraversal));
        assert_eq!(norm("a/.."), Err(UnsafeReason::ParentTraversal));
        assert_eq!(
            normalize_entry_path(Path::new(OsStr::from_bytes(b"a\0b"))),
            Err(UnsafeReason::InvalidName)
        );
    }

    #[test]
    fn non_utf8_names_pass() {
        let p = Path::new(OsStr::from_bytes(b"d\xff/f\xfe"));
        assert_eq!(normalize_entry_path(p).as_deref(), Ok(p));
    }

    #[test]
    fn link_targets_are_resolved_lexically() {
        let r = |l: &str, t: &str| resolve_link_target(Path::new(l), Path::new(t));
        assert_eq!(r("d/link", "file"), Ok("d/file".into()));
        assert_eq!(r("d/link", "../x/y"), Ok("x/y".into()));
        assert_eq!(r("link", "d/./e"), Ok("d/e".into()));
        assert_eq!(r("d/link", "../../x"), Err(UnsafeReason::LinkEscape));
        assert_eq!(r("d/link", "/etc"), Err(UnsafeReason::LinkEscape));
        assert_eq!(r("d/link", ""), Err(UnsafeReason::LinkEscape));
        assert_eq!(r("", "a/b"), Ok("a/b".into()));
    }
}
