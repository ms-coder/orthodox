//! File highlighting rule matcher: first matching rule wins.

use ofm_core::Entry;

use crate::theme::HighlightRule;

/// Resolved style for one entry (None fields = keep the panel default).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Style {
    /// Foreground (colour string or `@role` reference, as in the theme).
    pub fg: Option<String>,
    /// Background.
    pub bg: Option<String>,
    /// Bold.
    pub bold: Option<bool>,
    /// Icon name.
    pub icon: Option<String>,
}

/// Return the style of the first rule matching `entry` (attr tests: `dir`, `hidden` (dot-file),
/// `executable` (mode & 0o111, files only), `symlink`, `broken_symlink`, `archive`/`image`/`source`
/// by built-in extension sets; then `mask` globs via `ofm_core::mask`). `None` if no rule matches.
pub fn style_for(_rules: &[HighlightRule], _entry: &Entry) -> Option<Style> {
    todo!("T1.3")
}
