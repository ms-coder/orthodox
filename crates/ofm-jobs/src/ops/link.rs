//! Create links (T3.2 owns this file; F18). Symbolic: `fs.symlink(target, link)` with `target`
//! stored verbatim; hard: `fs.hard_link(target, link)` (refuse directories with a clear error).
//! An existing `link` is an error (never replaced silently); `ctx.note_created(link)` on success.

use std::path::{Component, Path, PathBuf};

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::spec::LinkKind;

/// Create `link` pointing at `target`.
pub fn run(
    _ctx: &mut JobCtx,
    _target: &Path,
    _link: &Path,
    _kind: LinkKind,
) -> Result<(), JobError> {
    todo!("T3.2")
}

/// Lexical relative path from the directory containing `link` to `target`, for "relative symlink"
/// in the link dialog: `relative_target("/a/b/l", "/a/c/t") == "../c/t"`. Both paths must be
/// absolute (otherwise `target` is returned unchanged); no file system access, `.`/`..` are not
/// resolved. Pure helper used by the UI.
pub fn relative_target(link: &Path, target: &Path) -> PathBuf {
    if !link.is_absolute() || !target.is_absolute() {
        return target.to_path_buf();
    }
    let base: Vec<Component<'_>> = link
        .parent()
        .map_or_else(Vec::new, |p| p.components().collect());
    let tgt: Vec<Component<'_>> = target.components().collect();
    let common = base.iter().zip(&tgt).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for c in &tgt[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_targets() {
        let r = |l: &str, t: &str| relative_target(Path::new(l), Path::new(t));
        assert_eq!(r("/a/b/l", "/a/c/t"), PathBuf::from("../c/t"));
        assert_eq!(r("/a/l", "/a/t"), PathBuf::from("t"));
        assert_eq!(r("/a/b/c/l", "/a/t"), PathBuf::from("../../t"));
        assert_eq!(r("/a/l", "/a"), PathBuf::from("."));
        assert_eq!(r("/a/b/l", "/a"), PathBuf::from(".."));
        assert_eq!(r("/a/l", "/a/l/x"), PathBuf::from("l/x"));
        assert_eq!(r("/l", "/t/u"), PathBuf::from("t/u"));
        assert_eq!(r("rel/l", "/t"), PathBuf::from("/t"));
    }

    #[test]
    #[ignore = "T3.2"]
    fn symlink_and_hardlink_creation() {}

    #[test]
    #[ignore = "T3.2"]
    fn existing_link_path_is_an_error() {}
}
