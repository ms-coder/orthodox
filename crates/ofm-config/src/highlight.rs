//! File highlighting rule matcher: first matching rule wins.
//!
//! Masks are matched with a small built-in glob (`*`, `?`, `;`-separated alternatives,
//! ASCII case-insensitive, on the raw name bytes) so this crate does not depend on the mask
//! engine of `ofm-core`.

use std::os::unix::ffi::OsStrExt;

use ofm_core::{Entry, EntryKind};

use crate::theme::{Color, HighlightRule};

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

const ARCHIVE_EXT: &[&str] = &[
    "zip", "7z", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "rar", "lz4", "cpio",
];
const IMAGE_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tif", "tiff", "ico", "avif", "heic",
];
const SOURCE_EXT: &[&str] = &[
    "rs", "c", "h", "cc", "cpp", "hpp", "cs", "py", "js", "ts", "java", "go", "rb", "sh", "pl",
    "lua", "php", "kt", "swift",
];

fn extension(name: &[u8]) -> Option<String> {
    let dot = name.iter().rposition(|b| *b == b'.')?;
    if dot == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&name[dot + 1..]).to_ascii_lowercase())
}

fn attr_matches(attr: &str, entry: &Entry) -> bool {
    let name = entry.name.as_bytes();
    let is_file = matches!(entry.kind, EntryKind::File);
    let has_ext = |set: &[&str]| {
        !entry.kind.is_dir_like() && extension(name).is_some_and(|e| set.contains(&e.as_str()))
    };
    match attr {
        "dir" => entry.kind.is_dir_like(),
        "hidden" => name.first() == Some(&b'.') && name != b"..",
        "executable" => is_file && entry.mode & 0o111 != 0,
        "symlink" => matches!(entry.kind, EntryKind::Symlink { .. }),
        "broken_symlink" => matches!(entry.kind, EntryKind::Symlink { target: None }),
        "archive" => has_ext(ARCHIVE_EXT),
        "image" => has_ext(IMAGE_EXT),
        "source" => has_ext(SOURCE_EXT),
        _ => false,
    }
}

/// Glob match of `pat` against `name`; `*` any run, `?` any single byte.
fn glob(pat: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pat.len() && pat[p] == b'*' {
            star = Some((p, n));
            p += 1;
        } else if p < pat.len() && (pat[p] == b'?' || pat[p].eq_ignore_ascii_case(&name[n])) {
            p += 1;
            n += 1;
        } else if let Some((sp, sn)) = star {
            p = sp + 1;
            n = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|b| *b == b'*')
}

fn mask_matches(mask: &str, entry: &Entry) -> bool {
    let name = entry.name.as_bytes();
    mask.split(';')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .any(|m| glob(m.as_bytes(), name))
}

fn rule_matches(rule: &HighlightRule, entry: &Entry) -> bool {
    (rule.attr.is_empty() || attr_matches(&rule.attr, entry))
        && (rule.mask.trim().is_empty() || mask_matches(&rule.mask, entry))
}

fn colour(c: &Option<Color>) -> Option<String> {
    c.as_ref().filter(|c| !c.0.is_empty()).map(|c| c.0.clone())
}

/// Return the style of the first rule matching `entry` (attr tests: `dir`, `hidden` (dot-file),
/// `executable` (mode & 0o111, files only), `symlink`, `broken_symlink`, `archive`/`image`/`source`
/// by built-in extension sets; then `mask` globs). A rule with neither `attr` nor `mask` matches
/// everything; an unknown `attr` never matches. `None` if no rule matches.
pub fn style_for(rules: &[HighlightRule], entry: &Entry) -> Option<Style> {
    let rule = rules.iter().find(|r| rule_matches(r, entry))?;
    Some(Style {
        fg: colour(&rule.fg),
        bg: colour(&rule.bg),
        bold: rule.bold,
        icon: rule.icon.clone().filter(|i| !i.is_empty()),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use ofm_core::entry::TargetKind;

    fn rule(attr: &str, mask: &str, fg: &str) -> HighlightRule {
        HighlightRule {
            attr: attr.into(),
            mask: mask.into(),
            fg: Some(Color(fg.into())),
            ..HighlightRule::default()
        }
    }

    fn file(name: &str, mode: u32) -> Entry {
        let mut e = Entry::new(name, EntryKind::File);
        e.mode = mode;
        e
    }

    #[test]
    fn globs() {
        assert!(glob(b"*.tar.*", b"a.tar.gz"));
        assert!(glob(b"*.ZIP", b"x.zip"));
        assert!(glob(b"a?c", b"abc") && !glob(b"a?c", b"ac"));
        assert!(glob(b"*", b"") && glob(b"a*b*c", b"aXXbYYc") && !glob(b"a*b", b"abc"));
    }

    #[test]
    fn first_match_wins_and_attrs() {
        let rules = [
            rule("broken_symlink", "", "#f00"),
            rule("dir", "", "#00f"),
            rule("hidden", "", "#088"),
            rule("executable", "", "#0f0"),
            rule("archive", "*.zip;*.tar.*", "#f0f"),
            rule("image", "", "#ff0"),
            rule("", "*.md", "#fff"),
        ];
        let fg = |e: &Entry| style_for(&rules, e).and_then(|s| s.fg);
        let broken = Entry::new("l", EntryKind::Symlink { target: None });
        assert_eq!(fg(&broken).as_deref(), Some("#f00"));
        let dirlink = Entry::new(
            "l",
            EntryKind::Symlink {
                target: Some(TargetKind::Dir),
            },
        );
        assert_eq!(fg(&dirlink).as_deref(), Some("#00f"));
        assert_eq!(
            fg(&Entry::new("d", EntryKind::Dir)).as_deref(),
            Some("#00f")
        );
        assert_eq!(fg(&file(".rc", 0o644)).as_deref(), Some("#088"));
        assert_eq!(fg(&file("run.sh", 0o755)).as_deref(), Some("#0f0"));
        // directories are not "executable"
        assert_eq!(fg(&file("a.zip", 0o644)).as_deref(), Some("#f0f"));
        assert_eq!(fg(&file("a.tar.gz", 0o644)).as_deref(), Some("#f0f"));
        assert_eq!(fg(&file("PIC.JPG", 0o644)).as_deref(), Some("#ff0"));
        assert_eq!(fg(&file("README.md", 0o644)).as_deref(), Some("#fff"));
        assert_eq!(fg(&file("data.bin", 0o644)), None);
        assert!(style_for(&[], &file("x", 0)).is_none());
    }

    #[test]
    fn empty_rule_matches_all_unknown_attr_none_and_both_must_match() {
        let all = [rule("", "", "#111")];
        assert!(style_for(&all, &file("x", 0)).is_some());
        let unknown = [rule("bogus", "", "#111")];
        assert!(style_for(&unknown, &file("x", 0)).is_none());
        let both = [rule("image", "*.png", "#111")];
        assert!(style_for(&both, &file("a.png", 0)).is_some());
        assert!(style_for(&both, &file("a.jpg", 0)).is_none());
    }

    #[test]
    fn non_utf8_names_work() {
        use std::os::unix::ffi::OsStringExt;
        let name = std::ffi::OsString::from_vec(vec![b'a', 0xff, b'.', b'z', b'i', b'p']);
        let e = Entry::new(name, EntryKind::File);
        assert!(style_for(&[rule("archive", "*.zip", "#111")], &e).is_some());
    }

    #[test]
    fn far_blue_theme_rules_apply() {
        let t = Theme::builtin("far-blue").unwrap();
        let s = style_for(&t.highlight, &file("run.sh", 0o755)).unwrap();
        assert_eq!(s.fg.as_deref(), Some("#55FF55"));
    }
}
