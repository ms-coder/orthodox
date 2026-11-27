//! The Find dialog's fields as plain data, and their translation into an `ofm_search::SearchQuery`.
//! Pure functions (no GTK, no file system): size and date parsing, mask lists, root lists. The
//! clock and the time zone are injected ([`LocalClock`]) so the date rules are unit-testable.

use std::path::{Path, PathBuf};

use ofm_search::{
    ContentMode, ContentQuery, NameFilter, NameMode, SearchEncoding, SearchError, SearchQuery,
    SizeFilter, TimeFilter,
};
use ofm_viewer::Encoding;

use crate::i18n::tr;
use crate::navigation::goto::resolve_input;

/// Results kept by one search before the engine stops (`SearchSummary::truncated`).
pub const MAX_HITS: usize = 1_000_000;

/// Which dialog field an error belongs to (the dialog focuses it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// File mask.
    Mask,
    /// Containing text.
    Text,
    /// Minimum size.
    SizeMin,
    /// Maximum size.
    SizeMax,
    /// Modified after.
    After,
    /// Modified before.
    Before,
    /// Search in.
    Roots,
}

/// A field value that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormError {
    /// The field.
    pub field: Field,
    /// What is wrong (user-presentable).
    pub message: String,
}

fn err(field: Field, message: impl Into<String>) -> FormError {
    FormError {
        field,
        message: message.into(),
    }
}

/// The encodings offered by the dialog, in menu order (`None` = automatic).
pub const ENCODING_CHOICES: [(Option<Encoding>, &str); 8] = [
    (None, "Auto"),
    (Some(Encoding::Utf8), "UTF-8"),
    (Some(Encoding::Utf16Le), "UTF-16 LE"),
    (Some(Encoding::Utf16Be), "UTF-16 BE"),
    (Some(Encoding::Windows1251), "CP1251"),
    (Some(Encoding::Ibm866), "CP866"),
    (Some(Encoding::Koi8R), "KOI8-R"),
    (Some(Encoding::Iso8859_1), "Latin-1"),
];

/// Everything the user can type or tick, as the dialog holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindForm {
    /// Name masks, `include [| exclude]`, items separated by `;` or `,`.
    pub mask: String,
    /// Name match is case-sensitive.
    pub mask_case: bool,
    /// "Containing text" (empty = name search only).
    pub text: String,
    /// The text is a regular expression.
    pub text_regex: bool,
    /// The text match is case-sensitive.
    pub text_case: bool,
    /// Match whole words only.
    pub whole_words: bool,
    /// Index into [`ENCODING_CHOICES`].
    pub encoding: usize,
    /// Minimum size ("10k", "1.5M"; empty = none).
    pub size_min: String,
    /// Maximum size.
    pub size_max: String,
    /// Modified after (date, date and time, or "7d").
    pub after: String,
    /// Modified before.
    pub before: String,
    /// Directories to search, separated by `;`.
    pub roots: String,
    /// Descend into subdirectories.
    pub subdirs: bool,
    /// Follow symbolic links to directories.
    pub follow_symlinks: bool,
    /// Do not cross file systems.
    pub same_filesystem: bool,
    /// Include hidden files.
    pub hidden: bool,
    /// Search inside archives.
    pub archives: bool,
}

impl Default for FindForm {
    fn default() -> Self {
        FindForm {
            mask: "*".to_owned(),
            mask_case: false,
            text: String::new(),
            text_regex: false,
            text_case: false,
            whole_words: false,
            encoding: 0,
            size_min: String::new(),
            size_max: String::new(),
            after: String::new(),
            before: String::new(),
            roots: String::new(),
            subdirs: true,
            follow_symlinks: false,
            same_filesystem: false,
            hidden: true,
            archives: false,
        }
    }
}

/// Wall clock and time zone, injected so tests do not depend on the machine.
pub trait LocalClock {
    /// Now, nanoseconds since the Unix epoch.
    fn now_ns(&self) -> i128;
    /// Unix seconds of a local calendar time; `None` for a time that does not exist.
    fn local_to_unix(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<i64>;
    /// Local calendar fields `(y, mo, d, h, mi, s)` of unix seconds.
    fn unix_to_local(&self, secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)>;
}

/// Context for [`FindForm::to_query`].
pub struct ParseCtx<'a> {
    /// Directory relative roots are taken from (the active panel's directory).
    pub base: &'a Path,
    /// Home directory for `~`.
    pub home: &'a Path,
    /// Clock and time zone.
    pub clock: &'a dyn LocalClock,
}

// ---- masks -------------------------------------------------------------------------------------

/// Split a mask list at `;` and at `,` outside braces (`*.{jpg,png}` stays one item); items are
/// trimmed, empty ones dropped.
pub fn split_masks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '{' => {
                depth += 1;
                cur.push(c);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ';' | ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The `ofm_core::mask` expression for `include | exclude`: `inc1;inc2;!exc1;!exc2`. An empty
/// include part means "everything" (the mask crate's empty include list).
pub fn mask_expression(text: &str) -> String {
    let (inc, exc) = match text.split_once('|') {
        Some((i, e)) => (i, e),
        None => (text, ""),
    };
    let mut parts = split_masks(inc);
    parts.extend(split_masks(exc).into_iter().map(|e| {
        if e.starts_with('!') {
            e
        } else {
            format!("!{e}")
        }
    }));
    parts.join(";")
}

// ---- roots -------------------------------------------------------------------------------------

/// Split the "search in" text at `;` or newlines, resolve `~` and relative items, clean `.`/`..`.
pub fn parse_roots(text: &str, base: &Path, home: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for item in text.split([';', '\n']) {
        let item = item.trim();
        if item.is_empty() || item.contains('\0') {
            continue;
        }
        let p = resolve_input(item, base, home);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

// ---- sizes -------------------------------------------------------------------------------------

/// Parse a size: digits with an optional fraction and an optional unit (`k`, `M`, `G`, `T`, with
/// optional `B` / `iB`; binary multiples). `None` for empty text.
pub fn parse_size(text: &str) -> Result<Option<u64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let bad = || format!("{} \"{t}\"", tr("Not a size:"));
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let num = num.replace(',', ".");
    if num.is_empty() {
        return Err(bad());
    }
    let unit = unit.trim().to_ascii_lowercase();
    let unit = unit
        .strip_suffix("ib")
        .or_else(|| unit.strip_suffix('b'))
        .unwrap_or(&unit);
    let mult: u64 = match unit {
        "" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => return Err(bad()),
    };
    if let Ok(n) = num.parse::<u64>() {
        return n.checked_mul(mult).map(Some).ok_or_else(bad);
    }
    let f: f64 = num.parse().map_err(|_| bad())?;
    if !f.is_finite() || f < 0.0 || f * (mult as f64) >= u64::MAX as f64 {
        return Err(bad());
    }
    Ok(Some((f * mult as f64).round() as u64))
}

// ---- dates -------------------------------------------------------------------------------------

const NS: i128 = 1_000_000_000;

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// `YYYY-MM-DD`, `YYYY/MM/DD` or `DD.MM.YYYY`.
fn parse_date_part(s: &str) -> Option<(i32, u32, u32)> {
    let nums =
        |sep: char| -> Option<Vec<u32>> { s.split(sep).map(|p| p.parse::<u32>().ok()).collect() };
    let (y, m, d) = if s.contains('.') {
        let n = nums('.')?;
        (*n.get(2)?, *n.get(1)?, *n.first()?)
    } else {
        let sep = if s.contains('/') { '/' } else { '-' };
        let n = nums(sep)?;
        if n.len() != 3 {
            return None;
        }
        (n[0], n[1], n[2])
    };
    if s.split(['-', '/', '.']).count() != 3 {
        return None;
    }
    let y = i32::try_from(y).ok()?;
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

/// `HH:MM` or `HH:MM:SS`.
fn parse_time_part(s: &str) -> Option<(u32, u32, u32)> {
    let n: Vec<u32> = s
        .split(':')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<_>>()?;
    let (h, m, sec) = match n.as_slice() {
        [h, m] => (*h, *m, 0),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    (h < 24 && m < 60 && sec < 60).then_some((h, m, sec))
}

/// A relative age: `7d`, `12h`, `2w`, `30min` (before now).
fn parse_age(s: &str) -> Option<i128> {
    let t = s.trim().trim_start_matches('-');
    let split = t.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = t.split_at(split);
    let n: i128 = n.parse().ok()?;
    let secs = match unit.trim() {
        "min" | "mi" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return None,
    };
    Some(n * secs * NS)
}

/// Parse a date field into nanoseconds since the epoch.
///
/// Accepted: `2026-11-02`, `2026/11/02`, `02.11.2026` (optionally followed by `HH:MM[:SS]`),
/// `today`, `yesterday`, `now`, and ages like `7d`, `12h`, `2w`. A date without a time is the
/// start of that day, or its last nanosecond when `end_of_day` (so "before 2026-11-02" includes
/// the whole day). Empty text is `None`.
pub fn parse_when(
    text: &str,
    clock: &dyn LocalClock,
    end_of_day: bool,
) -> Result<Option<i128>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let bad = || format!("{} \"{t}\"", tr("Not a date:"));
    let lower = t.to_ascii_lowercase();
    let now = clock.now_ns();
    let day_bounds = |y: i32, m: u32, d: u32| -> Option<i128> {
        let start = i128::from(clock.local_to_unix(y, m, d, 0, 0, 0)?) * NS;
        Some(if end_of_day {
            // Next midnight (DST days are not 24 hours long) minus one nanosecond.
            let (ny, nm, nd) = if d < days_in_month(y, m) {
                (y, m, d + 1)
            } else if m < 12 {
                (y, m + 1, 1)
            } else {
                (y + 1, 1, 1)
            };
            i128::from(clock.local_to_unix(ny, nm, nd, 0, 0, 0)?) * NS - 1
        } else {
            start
        })
    };
    let today = || -> Option<(i32, u32, u32)> {
        let (y, m, d, ..) = clock.unix_to_local(i64::try_from(now.div_euclid(NS)).ok()?)?;
        Some((y, m, d))
    };
    match lower.as_str() {
        "now" => return Ok(Some(now)),
        "today" => {
            let (y, m, d) = today().ok_or_else(bad)?;
            return day_bounds(y, m, d).map(Some).ok_or_else(bad);
        }
        "yesterday" => {
            let (y, m, d) = today().ok_or_else(bad)?;
            // Midnight of today minus a few hours is always inside yesterday.
            let t0 = i128::from(clock.local_to_unix(y, m, d, 0, 0, 0).ok_or_else(bad)?) * NS;
            let (yy, mm, dd, ..) = clock
                .unix_to_local(i64::try_from((t0 - 3600 * NS).div_euclid(NS)).map_err(|_| bad())?)
                .ok_or_else(bad)?;
            return day_bounds(yy, mm, dd).map(Some).ok_or_else(bad);
        }
        _ => {}
    }
    if let Some(age) = parse_age(&lower) {
        return Ok(Some(now - age));
    }
    let mut words = t.split(['T', ' ']).filter(|w| !w.is_empty());
    let date = words.next().and_then(parse_date_part).ok_or_else(bad)?;
    match words.next() {
        None => day_bounds(date.0, date.1, date.2).map(Some).ok_or_else(bad),
        Some(time) => {
            if words.next().is_some() {
                return Err(bad());
            }
            let (h, mi, s) = parse_time_part(time).ok_or_else(bad)?;
            let secs = clock
                .local_to_unix(date.0, date.1, date.2, h, mi, s)
                .ok_or_else(bad)?;
            let ns = i128::from(secs) * NS;
            // "before 12:30" with minute precision includes the whole minute.
            Ok(Some(if end_of_day && time.matches(':').count() == 1 {
                ns + 60 * NS - 1
            } else {
                ns
            }))
        }
    }
}

// ---- the query ---------------------------------------------------------------------------------

fn field_of(name: &str) -> Field {
    match name {
        "roots" => Field::Roots,
        "name" => Field::Mask,
        "content" => Field::Text,
        "size" => Field::SizeMax,
        "mtime" => Field::Before,
        _ => Field::Mask,
    }
}

impl FindForm {
    /// Build and validate the query. Errors name the field to focus.
    pub fn to_query(&self, cx: &ParseCtx<'_>) -> Result<SearchQuery, FormError> {
        let roots = parse_roots(&self.roots, cx.base, cx.home);
        if roots.is_empty() {
            return Err(err(Field::Roots, tr("Enter a directory to search in.")));
        }
        let mask = mask_expression(&self.mask);
        let size = SizeFilter {
            min: parse_size(&self.size_min).map_err(|m| err(Field::SizeMin, m))?,
            max: parse_size(&self.size_max).map_err(|m| err(Field::SizeMax, m))?,
        };
        let mtime = TimeFilter {
            after_ns: parse_when(&self.after, cx.clock, false).map_err(|m| err(Field::After, m))?,
            before_ns: parse_when(&self.before, cx.clock, true)
                .map_err(|m| err(Field::Before, m))?,
        };
        let content = (!self.text.is_empty()).then(|| ContentQuery {
            pattern: self.text.clone(),
            mode: if self.text_regex {
                ContentMode::Regex
            } else {
                ContentMode::Text
            },
            case_sensitive: self.text_case,
            whole_words: self.whole_words,
            encoding: ENCODING_CHOICES
                .get(self.encoding)
                .and_then(|(e, _)| *e)
                .map_or(SearchEncoding::Auto, SearchEncoding::Fixed),
            search_binary: false,
        });
        let query = SearchQuery {
            roots,
            name: NameFilter {
                pattern: mask,
                mode: NameMode::Glob,
                case_sensitive: self.mask_case,
            },
            content,
            size,
            mtime,
            follow_symlinks: self.follow_symlinks,
            same_filesystem: self.same_filesystem,
            include_hidden: self.hidden,
            search_archives: self.archives,
            max_depth: (!self.subdirs).then_some(0),
            max_hits: Some(MAX_HITS),
            ..SearchQuery::default()
        };
        query.validate().map_err(|e| match e {
            SearchError::InvalidQuery { field, message } => err(field_of(field), message),
            other => err(Field::Mask, other.to_string()),
        })?;
        Ok(query)
    }
}

/// A short description of a query for the panel title ("Find: *.rs").
pub fn title_for(form: &FindForm) -> String {
    let mask = form.mask.trim();
    match (mask.is_empty() || mask == "*", form.text.is_empty()) {
        (true, true) => tr("Search results").to_owned(),
        (false, true) => format!("{} {mask}", tr("Find:")),
        (true, false) => format!("{} \"{}\"", tr("Find:"), form.text),
        (false, false) => format!("{} {mask} \"{}\"", tr("Find:"), form.text),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// UTC clock fixed at 2026-11-22 12:00:00.
    struct Utc;

    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn civil_from_days(z: i64) -> (i64, i64, i64) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    impl LocalClock for Utc {
        fn now_ns(&self) -> i128 {
            i128::from(days_from_civil(2026, 11, 22) * 86_400 + 12 * 3600) * NS
        }
        fn local_to_unix(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<i64> {
            Some(
                days_from_civil(i64::from(y), i64::from(mo), i64::from(d)) * 86_400
                    + i64::from(h) * 3600
                    + i64::from(mi) * 60
                    + i64::from(s),
            )
        }
        fn unix_to_local(&self, secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
            let days = secs.div_euclid(86_400);
            let rem = secs.rem_euclid(86_400);
            let (y, m, d) = civil_from_days(days);
            Some((
                y as i32,
                m as u32,
                d as u32,
                (rem / 3600) as u32,
                (rem % 3600 / 60) as u32,
                (rem % 60) as u32,
            ))
        }
    }

    fn at(y: i64, m: i64, d: i64, h: i64, mi: i64, s: i64) -> i128 {
        i128::from(days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s) * NS
    }

    #[test]
    fn mask_lists_respect_braces_and_exclusions() {
        assert_eq!(
            split_masks("*.rs; *.toml,*.{jpg,png} ,"),
            ["*.rs", "*.toml", "*.{jpg,png}"]
        );
        assert_eq!(
            mask_expression("*.rs;*.toml | *.bak, target*"),
            "*.rs;*.toml;!*.bak;!target*"
        );
        assert_eq!(mask_expression("*|"), "*");
        assert_eq!(mask_expression("| *.tmp"), "!*.tmp");
        assert_eq!(mask_expression("  "), "");
        assert_eq!(mask_expression("a|!b"), "a;!b");
    }

    #[test]
    fn roots_resolve_and_dedupe() {
        let r = parse_roots(
            " /etc ; ~/src;docs;; /etc;../x ",
            Path::new("/work/p"),
            Path::new("/home/u"),
        );
        let want: Vec<PathBuf> = ["/etc", "/home/u/src", "/work/p/docs", "/work/x"]
            .map(PathBuf::from)
            .into();
        assert_eq!(r, want);
        assert!(parse_roots("  ;; ", Path::new("/"), Path::new("/h")).is_empty());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("").unwrap(), None);
        assert_eq!(parse_size("  ").unwrap(), None);
        assert_eq!(parse_size("100").unwrap(), Some(100));
        assert_eq!(parse_size("10k").unwrap(), Some(10 * 1024));
        assert_eq!(parse_size("10 KB").unwrap(), Some(10 * 1024));
        assert_eq!(parse_size("2MiB").unwrap(), Some(2 << 20));
        assert_eq!(parse_size("1.5M").unwrap(), Some(1_572_864));
        assert_eq!(parse_size("1,5k").unwrap(), Some(1536));
        assert_eq!(parse_size("3g").unwrap(), Some(3 << 30));
        assert_eq!(parse_size("5b").unwrap(), Some(5));
        for bad in ["k", "-5", "abc", "10x", "1.2.3", "99999999999999999999t"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn dates_absolute_relative_and_end_of_day() {
        let c = &Utc;
        let w = |s: &str, end: bool| parse_when(s, c, end).unwrap();
        assert_eq!(w("", false), None);
        assert_eq!(w("2026-11-02", false), Some(at(2026, 11, 2, 0, 0, 0)));
        assert_eq!(w("2026-11-02", true), Some(at(2026, 11, 3, 0, 0, 0) - 1));
        assert_eq!(w("2026/11/30", true), Some(at(2026, 12, 1, 0, 0, 0) - 1));
        assert_eq!(w("2026-12-31", true), Some(at(2027, 1, 1, 0, 0, 0) - 1));
        assert_eq!(w("02.11.2026", false), Some(at(2026, 11, 2, 0, 0, 0)));
        assert_eq!(
            w("2026-11-02 14:30", false),
            Some(at(2026, 11, 2, 14, 30, 0))
        );
        assert_eq!(
            w("2026-11-02T14:30:15", false),
            Some(at(2026, 11, 2, 14, 30, 15))
        );
        assert_eq!(
            w("2026-11-02 14:30", true),
            Some(at(2026, 11, 2, 14, 31, 0) - 1)
        );
        assert_eq!(w("today", false), Some(at(2026, 11, 22, 0, 0, 0)));
        assert_eq!(w("yesterday", false), Some(at(2026, 11, 21, 0, 0, 0)));
        assert_eq!(w("now", false), Some(at(2026, 11, 22, 12, 0, 0)));
        assert_eq!(w("7d", false), Some(at(2026, 11, 15, 12, 0, 0)));
        assert_eq!(w("2w", false), Some(at(2026, 11, 8, 12, 0, 0)));
        assert_eq!(w("12h", false), Some(at(2026, 11, 22, 0, 0, 0)));
        // 2028 is a leap year.
        assert_eq!(w("2028-02-29", false), Some(at(2028, 2, 29, 0, 0, 0)));
        for bad in [
            "2027-02-29",
            "2026-13-01",
            "2026-00-10",
            "11/22",
            "soon",
            "2026-11-02 25:00",
            "2026-11-02 10:00 extra",
            "7x",
        ] {
            assert!(parse_when(bad, c, false).is_err(), "{bad}");
        }
    }

    fn cx<'a>(clock: &'a Utc) -> ParseCtx<'a> {
        ParseCtx {
            base: Path::new("/work"),
            home: Path::new("/home/u"),
            clock,
        }
    }

    #[test]
    fn query_from_a_full_form() {
        let form = FindForm {
            mask: "*.rs | *.bak".into(),
            mask_case: true,
            text: "fn main".into(),
            text_regex: true,
            text_case: true,
            whole_words: true,
            encoding: 4,
            size_min: "1k".into(),
            size_max: "2M".into(),
            after: "2026-01-01".into(),
            before: "2026-06-30".into(),
            roots: "~/a;/b".into(),
            subdirs: false,
            follow_symlinks: true,
            same_filesystem: true,
            hidden: false,
            archives: true,
        };
        let q = form.to_query(&cx(&Utc)).unwrap();
        assert_eq!(q.roots, [PathBuf::from("/home/u/a"), PathBuf::from("/b")]);
        assert_eq!(q.name.pattern, "*.rs;!*.bak");
        assert!(q.name.case_sensitive);
        let c = q.content.as_ref().unwrap();
        assert_eq!(c.pattern, "fn main");
        assert_eq!(c.mode, ContentMode::Regex);
        assert!(c.case_sensitive && c.whole_words);
        assert_eq!(c.encoding, SearchEncoding::Fixed(Encoding::Windows1251));
        assert_eq!((q.size.min, q.size.max), (Some(1024), Some(2 << 20)));
        assert_eq!(q.mtime.after_ns, Some(at(2026, 1, 1, 0, 0, 0)));
        assert_eq!(q.mtime.before_ns, Some(at(2026, 7, 1, 0, 0, 0) - 1));
        assert_eq!(q.max_depth, Some(0));
        assert!(q.follow_symlinks && q.same_filesystem && !q.include_hidden && q.search_archives);
        assert_eq!(q.max_hits, Some(MAX_HITS));
    }

    #[test]
    fn default_form_searches_names_in_depth() {
        let form = FindForm {
            roots: "/srv".into(),
            ..FindForm::default()
        };
        let q = form.to_query(&cx(&Utc)).unwrap();
        assert!(q.content.is_none() && q.max_depth.is_none() && q.include_hidden);
        assert_eq!(q.name.pattern, "*");
        assert!(!q.name.case_sensitive);
    }

    #[test]
    fn errors_name_the_field() {
        let ok = FindForm {
            roots: "/srv".into(),
            ..FindForm::default()
        };
        let field = |f: FindForm| f.to_query(&cx(&Utc)).unwrap_err().field;
        assert_eq!(
            field(FindForm {
                roots: " ; ".into(),
                ..ok.clone()
            }),
            Field::Roots
        );
        assert_eq!(
            field(FindForm {
                size_min: "x".into(),
                ..ok.clone()
            }),
            Field::SizeMin
        );
        assert_eq!(
            field(FindForm {
                size_max: "y".into(),
                ..ok.clone()
            }),
            Field::SizeMax
        );
        assert_eq!(
            field(FindForm {
                after: "never".into(),
                ..ok.clone()
            }),
            Field::After
        );
        assert_eq!(
            field(FindForm {
                before: "never".into(),
                ..ok.clone()
            }),
            Field::Before
        );
        assert_eq!(
            field(FindForm {
                size_min: "2k".into(),
                size_max: "1k".into(),
                ..ok.clone()
            }),
            Field::SizeMax
        );
        assert_eq!(
            field(FindForm {
                mask: "*.{rs".into(),
                ..ok.clone()
            }),
            Field::Mask
        );
        assert_eq!(
            field(FindForm {
                after: "2026-05-01".into(),
                before: "2026-01-01".into(),
                ..ok
            }),
            Field::Before
        );
    }

    #[test]
    fn panel_titles() {
        let f = |mask: &str, text: &str| {
            title_for(&FindForm {
                mask: mask.into(),
                text: text.into(),
                ..FindForm::default()
            })
        };
        assert_eq!(f("*", ""), "Search results");
        assert_eq!(f("*.rs", ""), "Find: *.rs");
        assert_eq!(f("*", "todo"), "Find: \"todo\"");
        assert_eq!(f("*.rs", "todo"), "Find: *.rs \"todo\"");
    }
}
