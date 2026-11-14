//! File masks (owned by T1.1).
//!
//! Syntax: patterns separated by `;`: `*.txt;*.md`, brace groups `*.{jpg,png}`,
//! `?` and `*` wildcards, `[abc]` classes, leading `!` on a pattern = exclude (`*;!*.tmp`).
//! A name matches if any include pattern matches and no exclude pattern matches (an empty include
//! list means "everything"). Matching is on the file name bytes, never on a lossy `String`.

use std::ffi::{OsStr, OsString};

/// Mask parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MaskError {
    /// Malformed pattern (unbalanced brace/bracket, empty).
    #[error("invalid mask {mask:?}: {reason}")]
    Invalid {
        /// The mask text.
        mask: String,
        /// Reason.
        reason: String,
    },
}

/// Options for [`Mask::parse`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MaskOpts {
    /// Match case-insensitively.
    pub case_insensitive: bool,
}

/// A compiled selection/filter mask.
#[derive(Debug, Clone)]
pub struct Mask {
    #[allow(dead_code, reason = "T1.1 fills in the compiled form")]
    source: String,
}

impl Mask {
    /// Compile a mask expression.
    pub fn parse(_mask: &str, _opts: MaskOpts) -> Result<Mask, MaskError> {
        todo!("T1.1")
    }
    /// True if `name` matches.
    pub fn is_match(&self, _name: &OsStr) -> bool {
        todo!("T1.1")
    }
    /// The original text.
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Rename mask for copy/move dialogs: `*.txt` -> `*.bak`, i.e. source pattern + target pattern.
/// Reserved `[N]`/`[C]` placeholders are v1.1.
#[derive(Debug, Clone)]
pub struct RenameMask {
    #[allow(dead_code, reason = "T1.1 fills in the compiled form")]
    target: String,
}

impl RenameMask {
    /// Parse the destination-name pattern (e.g. `*.bak`, `backup_*`, `*`).
    pub fn parse(_target: &str) -> Result<RenameMask, MaskError> {
        todo!("T1.1")
    }
    /// Apply to a source name: each `*` is replaced by the source stem (or whole name without a dot
    /// in the pattern), so `a.txt` + `*.bak` = `a.bak`; `?` copies the char at the same position.
    pub fn apply(&self, _source_name: &OsStr) -> OsString {
        todo!("T1.1")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T1.1"]
    fn mask_basics() {
        // *.{jpg,png} matches a.JPG when case-insensitive; !*.tmp excludes.
    }
}
