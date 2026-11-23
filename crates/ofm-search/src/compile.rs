//! Pattern compilation (private). `SearchQuery::validate` uses the same builders so a bad
//! pattern is rejected before the search starts; the engine compiles once more for itself.

use std::ffi::OsStr;

use ofm_core::mask::{Mask, MaskOpts};
use regex::bytes::RegexBuilder as BytesRegexBuilder;
use regex::{Regex, RegexBuilder};

use crate::error::SearchError;
use crate::query::{ContentQuery, NameFilter, NameMode, SearchEncoding};

/// Compiled name filter: an `ofm_core::mask` expression or a regex over the raw name bytes.
#[derive(Debug)]
pub(crate) enum NameMatcher {
    /// Glob list (`*.rs;*.md`, `!*.tmp`).
    Mask(Mask),
    /// Unanchored regex matched against the file name's raw bytes (non-UTF-8 names never
    /// match `\w`-style classes, but literal ASCII patterns work).
    Regex(regex::bytes::Regex),
}

impl NameMatcher {
    /// Compile from the query's name filter.
    pub(crate) fn new(filter: &NameFilter) -> Result<NameMatcher, SearchError> {
        match filter.mode {
            NameMode::Glob => {
                let opts = MaskOpts {
                    case_insensitive: !filter.case_sensitive,
                };
                Mask::parse(&filter.pattern, opts)
                    .map(NameMatcher::Mask)
                    .map_err(|e| SearchError::InvalidQuery {
                        field: "name",
                        message: e.to_string(),
                    })
            }
            NameMode::Regex => Ok(NameMatcher::Regex(compile_name_regex(
                &filter.pattern,
                filter.case_sensitive,
            )?)),
        }
    }

    /// True if `name` passes the filter. An empty pattern matches every name.
    pub(crate) fn is_match(&self, name: &OsStr) -> bool {
        match self {
            NameMatcher::Mask(m) => m.is_match(name),
            NameMatcher::Regex(re) => re.is_match(name.as_encoded_bytes()),
        }
    }
}

/// Compile a name regex (bytes mode, unanchored, case option). Also used by `validate`.
pub(crate) fn compile_name_regex(
    pattern: &str,
    case_sensitive: bool,
) -> Result<regex::bytes::Regex, SearchError> {
    BytesRegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| SearchError::InvalidQuery {
            field: "name",
            message: e.to_string(),
        })
}

/// Compile the content pattern into a Unicode regex over decoded lines (grep semantics:
/// literal text is escaped, `^`/`$` anchor to the line because each line is matched on its
/// own, `.` never crosses a line break). Also used by `validate`.
pub(crate) fn compile_line_regex(query: &ContentQuery) -> Result<Regex, SearchError> {
    let pattern = match query.mode {
        crate::query::ContentMode::Text => regex::escape(&query.pattern),
        crate::query::ContentMode::Regex => query.pattern.clone(),
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!query.case_sensitive)
        .build()
        .map_err(|e| SearchError::InvalidQuery {
            field: "content",
            message: e.to_string(),
        })
}

/// Everything the content matcher needs, compiled once per search.
#[derive(Debug)]
pub(crate) struct ContentSpec {
    /// Unicode regex applied to each decoded line.
    pub(crate) re: Regex,
    /// Whole-words option (checked on each match, grep `-w` style).
    pub(crate) whole_words: bool,
    /// Encoding selection (auto-detect or fixed).
    pub(crate) encoding: SearchEncoding,
    /// Search files that look binary too.
    pub(crate) search_binary: bool,
    /// Bytes read per chunk (tests lower this to exercise boundaries).
    pub(crate) chunk: usize,
    /// Raw bytes buffered for one line before it is matched in overlapping segments
    /// (tests lower this to exercise the segment path).
    pub(crate) max_line: usize,
}

impl ContentSpec {
    /// Compile from the query with production chunk/line sizes.
    pub(crate) fn new(query: &ContentQuery) -> Result<ContentSpec, SearchError> {
        Ok(ContentSpec {
            re: compile_line_regex(query)?,
            whole_words: query.whole_words,
            encoding: query.encoding,
            search_binary: query.search_binary,
            chunk: crate::content::DEFAULT_CHUNK,
            max_line: crate::content::DEFAULT_MAX_LINE,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::query::{ContentMode, ContentQuery, NameFilter, NameMode};
    use std::ffi::OsStr;

    fn name_filter(pattern: &str, mode: NameMode, case: bool) -> NameFilter {
        NameFilter {
            pattern: pattern.into(),
            mode,
            case_sensitive: case,
        }
    }

    #[test]
    fn glob_and_regex_names() {
        let g = NameMatcher::new(&name_filter("*.rs;!main.rs", NameMode::Glob, false)).unwrap();
        assert!(g.is_match(OsStr::new("lib.rs")));
        assert!(g.is_match(OsStr::new("LIB.RS"))); // include folds case
        assert!(!g.is_match(OsStr::new("main.rs")));
        assert!(!g.is_match(OsStr::new("MAIN.RS"))); // exclude folds case too

        let r = NameMatcher::new(&name_filter(r"^test_\d+", NameMode::Regex, true)).unwrap();
        assert!(r.is_match(OsStr::new("test_42")));
        assert!(!r.is_match(OsStr::new("xtest_42"))); // anchored
        assert!(!r.is_match(OsStr::new("TEST_42"))); // case-sensitive
    }

    #[test]
    fn bad_patterns_are_invalid_query() {
        let err = NameMatcher::new(&name_filter("*.{rs", NameMode::Glob, false)).unwrap_err();
        assert!(matches!(
            err,
            SearchError::InvalidQuery { field: "name", .. }
        ));
        let err = NameMatcher::new(&name_filter("(unclosed", NameMode::Regex, false)).unwrap_err();
        assert!(matches!(
            err,
            SearchError::InvalidQuery { field: "name", .. }
        ));
    }

    #[test]
    fn literal_text_is_escaped() {
        let q = ContentQuery {
            pattern: "a.b*c".into(),
            mode: ContentMode::Text,
            case_sensitive: true,
            whole_words: false,
            encoding: SearchEncoding::Auto,
            search_binary: false,
        };
        let spec = ContentSpec::new(&q).unwrap();
        assert!(spec.re.is_match("xa.b*cy"));
        assert!(!spec.re.is_match("aXbZc")); // '.' is literal now
    }

    #[test]
    fn regex_mode_and_case() {
        let mut q = ContentQuery::text("hello");
        q.mode = ContentMode::Regex;
        q.pattern = r"h(el)?lo\b".into();
        let spec = ContentSpec::new(&q).unwrap();
        assert!(spec.re.is_match("say hlO now")); // case-insensitive by default
        assert!(!spec.re.is_match("say hlx now"));
    }
}
