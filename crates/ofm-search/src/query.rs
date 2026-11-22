//! What to search for (the Alt+F7 dialog, as plain data).

use std::path::PathBuf;

use ofm_core::mask::{Mask, MaskOpts};
use ofm_viewer::Encoding;

use crate::error::SearchError;

/// How `NameFilter::pattern` is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameMode {
    /// `ofm_core::mask` syntax: `*.rs;*.toml`, `*.{jpg,png}`, `!*.tmp`.
    #[default]
    Glob,
    /// A regular expression (`regex` crate syntax), unanchored, matched against the file name.
    Regex,
}

/// File-name filter; matched against the **name only** (last component), never the whole path.
/// An empty pattern matches every name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameFilter {
    /// Pattern text.
    pub pattern: String,
    /// Glob list or regex.
    pub mode: NameMode,
    /// Case-sensitive match (Far default: insensitive).
    pub case_sensitive: bool,
}

impl Default for NameFilter {
    fn default() -> Self {
        NameFilter {
            pattern: String::new(),
            mode: NameMode::Glob,
            case_sensitive: false,
        }
    }
}

/// How `ContentQuery::pattern` is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentMode {
    /// Literal text.
    #[default]
    Text,
    /// Regular expression, applied line by line (no multi-line matches in v1.0).
    Regex,
}

/// Text encoding of searched files. The concrete encodings are the viewer's
/// (`ofm_viewer::Encoding`: UTF-8, UTF-16LE/BE, windows-1251, IBM866, KOI8-R, ISO-8859-1), so
/// "F3 on a hit" opens the viewer with the same encoding and byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchEncoding {
    /// Per file via `ofm_viewer::detect_encoding` (BOM, UTF-8, heuristics). Binary files are
    /// skipped unless `ContentQuery::search_binary`.
    #[default]
    Auto,
    /// Treat every file as this encoding.
    Fixed(Encoding),
}

/// Content ("find text in files") filter. Only regular files are searched; directories never
/// match when a content query is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentQuery {
    /// Text or regex. Must not be empty.
    pub pattern: String,
    /// Text or regex.
    pub mode: ContentMode,
    /// Case-sensitive match.
    pub case_sensitive: bool,
    /// Match whole words only.
    pub whole_words: bool,
    /// Encoding; the pattern is searched in decoded text.
    pub encoding: SearchEncoding,
    /// Search files that look binary too (NUL bytes in the first 8 KiB) instead of skipping them.
    pub search_binary: bool,
}

impl ContentQuery {
    /// Case-insensitive literal text, auto encoding.
    pub fn text(pattern: impl Into<String>) -> Self {
        ContentQuery {
            pattern: pattern.into(),
            mode: ContentMode::Text,
            case_sensitive: false,
            whole_words: false,
            encoding: SearchEncoding::Auto,
            search_binary: false,
        }
    }
}

/// Size range (bytes, inclusive); applies to files, ignored for directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SizeFilter {
    /// Minimum size.
    pub min: Option<u64>,
    /// Maximum size.
    pub max: Option<u64>,
}

/// Modification-time range in ns since the epoch (inclusive). The UI converts "last 7 days" to an
/// absolute bound with its own clock, so the engine never reads the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeFilter {
    /// Modified at or after.
    pub after_ns: Option<i128>,
    /// Modified at or before.
    pub before_ns: Option<i128>,
}

/// A complete search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Directories to search (absolute, local); a root may also be a single file.
    pub roots: Vec<PathBuf>,
    /// Name filter.
    pub name: NameFilter,
    /// Content filter; `None` = name/size/date only.
    pub content: Option<ContentQuery>,
    /// Report directories whose name matches (ignored when `content` is set).
    pub match_dirs: bool,
    /// Size filter.
    pub size: SizeFilter,
    /// Modification-time filter.
    pub mtime: TimeFilter,
    /// Descend into symlinked directories (loops are detected by (dev, ino) and skipped with a
    /// warning).
    pub follow_symlinks: bool,
    /// Do not cross into other file systems (`st_dev` of the root).
    pub same_filesystem: bool,
    /// Include hidden files and directories (dot-names). Default true: unlike `ripgrep`, a file
    /// manager search never hides things, and ignore files (`.gitignore`) are never honoured.
    pub include_hidden: bool,
    /// Also search inside archives (zip, tar.*, 7z; one level, no nesting). Archive hits are
    /// `Location::Archive`; encrypted archives are skipped with a `Warning` (a search never prompts).
    pub search_archives: bool,
    /// Maximum directory depth below a root (`Some(0)` = only the root's direct children).
    pub max_depth: Option<u32>,
    /// Stop after this many hits (`Done.truncated = true`); `None` = unlimited.
    pub max_hits: Option<usize>,
    /// Worker threads; 0 = automatic (min(4, available cores)).
    pub threads: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        SearchQuery {
            roots: Vec::new(),
            name: NameFilter::default(),
            content: None,
            match_dirs: true,
            size: SizeFilter::default(),
            mtime: TimeFilter::default(),
            follow_symlinks: false,
            same_filesystem: false,
            include_hidden: true,
            search_archives: false,
            max_depth: None,
            max_hits: None,
            threads: 0,
        }
    }
}

fn invalid(field: &'static str, message: impl Into<String>) -> SearchError {
    SearchError::InvalidQuery {
        field,
        message: message.into(),
    }
}

impl SearchQuery {
    /// Check the query without touching the file system, so the dialog can show errors before
    /// starting. Done here: roots present and absolute, glob mask parses, ranges ordered, content
    /// pattern non-empty. T7.1 adds: compile the name regex and the content pattern.
    pub fn validate(&self) -> Result<(), SearchError> {
        if self.roots.is_empty() {
            return Err(invalid("roots", "no directory to search"));
        }
        if let Some(r) = self.roots.iter().find(|r| !r.is_absolute()) {
            return Err(invalid("roots", format!("not absolute: {}", r.display())));
        }
        if self.name.mode == NameMode::Glob {
            let opts = MaskOpts {
                case_insensitive: !self.name.case_sensitive,
            };
            Mask::parse(&self.name.pattern, opts).map_err(|e| invalid("name", e.to_string()))?;
        }
        if let (Some(min), Some(max)) = (self.size.min, self.size.max)
            && min > max
        {
            return Err(invalid("size", "minimum is larger than maximum"));
        }
        if let (Some(a), Some(b)) = (self.mtime.after_ns, self.mtime.before_ns)
            && a > b
        {
            return Err(invalid("mtime", "'after' is later than 'before'"));
        }
        if let Some(c) = &self.content
            && c.pattern.is_empty()
        {
            return Err(invalid("content", "empty search text"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> SearchQuery {
        SearchQuery {
            roots: vec!["/srv".into()],
            ..SearchQuery::default()
        }
    }

    #[test]
    fn default_query_with_a_root_is_valid() {
        assert!(q().validate().is_ok());
    }

    #[test]
    fn validation_names_the_offending_field() {
        let field = |q: SearchQuery| match q.validate() {
            Err(SearchError::InvalidQuery { field, .. }) => field,
            other => panic!("expected InvalidQuery, got {other:?}"),
        };
        assert_eq!(field(SearchQuery::default()), "roots");
        let mut bad = q();
        bad.roots = vec!["rel".into()];
        assert_eq!(field(bad), "roots");
        let mut bad = q();
        bad.name.pattern = "*.{rs".into();
        assert_eq!(field(bad), "name");
        let mut bad = q();
        bad.size = SizeFilter {
            min: Some(10),
            max: Some(1),
        };
        assert_eq!(field(bad), "size");
        let mut bad = q();
        bad.mtime = TimeFilter {
            after_ns: Some(5),
            before_ns: Some(1),
        };
        assert_eq!(field(bad), "mtime");
        let mut bad = q();
        bad.content = Some(ContentQuery::text(""));
        assert_eq!(field(bad), "content");
    }
}
