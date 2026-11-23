//! Hit -> result-row mapping: the cell texts of the results list and the targets of "go to",
//! F3 and F4. Pure functions over `ofm_search::Hit`; names are bytes, shown lossily.

use std::ffi::OsString;
use std::ops::Range;
use std::path::{Path, PathBuf};

use ofm_core::{EntryKind, Location, TargetKind};
use ofm_search::Hit;

use crate::widgets::file_list::SizeFormat;

/// Texts of one result row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cells {
    /// File name (directories get a trailing `/`).
    pub name: String,
    /// Containing folder (`archive.zip#/dir` for hits inside archives).
    pub folder: String,
    /// Size (empty for directories).
    pub size: String,
    /// Modification time.
    pub mtime: String,
    /// `line N: snippet` for content hits, else empty.
    pub matched: String,
    /// Byte range of the match inside `matched`.
    pub match_range: Option<Range<usize>>,
}

/// Size text, 9 characters at most (same notations as the panels).
pub fn size_text(bytes: u64, fmt: SizeFormat) -> String {
    let (base, units): (f64, [&str; 6]) = match fmt {
        SizeFormat::Bytes => return bytes.to_string(),
        SizeFormat::Binary => (1024.0, ["B", "KiB", "MiB", "GiB", "TiB", "PiB"]),
        SizeFormat::Si => (1000.0, ["B", "kB", "MB", "GB", "TB", "PB"]),
    };
    if (bytes as f64) < base {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= base && unit + 1 < units.len() {
        value /= base;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", units[unit])
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

fn is_dir(hit: &Hit) -> bool {
    matches!(
        hit.entry.kind,
        EntryKind::Dir
            | EntryKind::Symlink {
                target: Some(TargetKind::Dir)
            }
    )
}

/// The folder a hit lives in, for display.
pub fn folder_of(hit: &Hit) -> String {
    match &hit.location {
        Location::Local(p) => p
            .parent()
            .map_or_else(String::new, |d| d.to_string_lossy().into_owned()),
        Location::Archive { archive, inner } => format!(
            "{}#/{}",
            archive.to_string_lossy(),
            inner.parent().unwrap_or(Path::new("")).to_string_lossy()
        ),
        other => other.display_lossy(),
    }
}

/// Texts of `hit`; `time` formats the modification time of a hit (seconds since the epoch ->
/// text), injected so this stays free of glib.
pub fn cells(hit: &Hit, size_fmt: SizeFormat, time: &dyn Fn(i64) -> String) -> Cells {
    let mut name = hit.entry.name.to_string_lossy().into_owned();
    let dir = is_dir(hit);
    if dir {
        name.push('/');
    }
    let secs = hit.entry.mtime_ns.div_euclid(1_000_000_000);
    let (matched, match_range) = match &hit.content {
        Some(c) => {
            let prefix = format!("{}: ", c.first.line);
            let range = c.first.snippet_match.clone();
            let shifted = prefix.len() + range.start..prefix.len() + range.end;
            let shifted = (range.end <= c.first.snippet.len()).then_some(shifted);
            (format!("{prefix}{}", c.first.snippet), shifted)
        }
        None => (String::new(), None),
    };
    Cells {
        name,
        folder: folder_of(hit),
        size: if dir {
            String::new()
        } else {
            size_text(hit.entry.size, size_fmt)
        },
        mtime: i64::try_from(secs).map(time).unwrap_or_default(),
        matched,
        match_range,
    }
}

/// Where "go to" opens: the containing directory and the name to put the cursor on.
pub fn goto_target(hit: &Hit) -> Option<(Location, OsString)> {
    match &hit.location {
        Location::Local(p) => Some((
            Location::Local(p.parent()?.to_owned()),
            p.file_name()?.to_owned(),
        )),
        Location::Archive { archive, inner } => Some((
            Location::Archive {
                archive: archive.clone(),
                inner: inner.parent().unwrap_or(Path::new("")).to_owned(),
            },
            inner.file_name()?.to_owned(),
        )),
        Location::List { .. } => None,
    }
}

/// Local file F3 / F4 act on, and for a content hit the byte offset of the first match.
pub fn file_target(hit: &Hit) -> Option<(PathBuf, Option<u64>)> {
    let path = hit.real_path()?.to_owned();
    if is_dir(hit) {
        return None;
    }
    Some((path, hit.content.as_ref().map(|c| c.first.byte_offset)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use ofm_core::Entry;
    use ofm_search::{ContentHit, ContentMatch};

    use super::*;

    fn local(path: &str, kind: EntryKind, size: u64) -> Hit {
        let p = PathBuf::from(path);
        let mut e = Entry::new(p.file_name().unwrap(), kind);
        e.size = size;
        e.mtime_ns = 1_700_000_000_000_000_000;
        e.real_path = Some(p.clone());
        Hit {
            location: Location::Local(p),
            entry: e,
            content: None,
        }
    }

    fn fmt_time(s: i64) -> String {
        format!("t{s}")
    }

    #[test]
    fn plain_file_row() {
        let c = cells(
            &local("/srv/src/a.rs", EntryKind::File, 2048),
            SizeFormat::Binary,
            &fmt_time,
        );
        assert_eq!(c.name, "a.rs");
        assert_eq!(c.folder, "/srv/src");
        assert_eq!(c.size, "2.0 KiB");
        assert_eq!(c.mtime, "t1700000000");
        assert!(c.matched.is_empty() && c.match_range.is_none());
    }

    #[test]
    fn directories_have_a_slash_and_no_size() {
        let c = cells(
            &local("/srv/src", EntryKind::Dir, 4096),
            SizeFormat::Binary,
            &fmt_time,
        );
        assert_eq!((c.name.as_str(), c.size.as_str()), ("src/", ""));
        assert!(file_target(&local("/srv/src", EntryKind::Dir, 0)).is_none());
    }

    #[test]
    fn content_hit_shows_line_and_snippet_with_shifted_match() {
        let mut h = local("/srv/a.txt", EntryKind::File, 10);
        h.content = Some(ContentHit {
            first: ContentMatch {
                line: 42,
                byte_offset: 1000,
                snippet: "let needle = 1;".into(),
                snippet_match: 4..10,
            },
            count: 3,
        });
        let c = cells(&h, SizeFormat::Bytes, &fmt_time);
        assert_eq!(c.matched, "42: let needle = 1;");
        let r = c.match_range.unwrap();
        assert_eq!(&c.matched[r], "needle");
        assert_eq!(c.size, "10");
        assert_eq!(
            file_target(&h),
            Some((PathBuf::from("/srv/a.txt"), Some(1000)))
        );
    }

    #[test]
    fn out_of_range_match_is_dropped_not_panicking() {
        let mut h = local("/a", EntryKind::File, 1);
        h.content = Some(ContentHit {
            first: ContentMatch {
                line: 1,
                byte_offset: 0,
                snippet: "abc".into(),
                snippet_match: 2..9,
            },
            count: 1,
        });
        assert!(
            cells(&h, SizeFormat::Bytes, &fmt_time)
                .match_range
                .is_none()
        );
    }

    #[test]
    fn non_utf8_names_display_lossy_and_operate_exactly() {
        let raw = PathBuf::from(OsString::from_vec(b"/d/\xff\xfe.txt".to_vec()));
        let h = local(raw.to_str().map_or("/d/x", |_| "/d/x"), EntryKind::File, 1);
        let mut h = h;
        h.entry.name = raw.file_name().unwrap().to_owned();
        h.entry.real_path = Some(raw.clone());
        h.location = Location::Local(raw.clone());
        let c = cells(&h, SizeFormat::Bytes, &fmt_time);
        assert!(c.name.contains('\u{fffd}'));
        let (loc, name) = goto_target(&h).unwrap();
        assert_eq!(loc, Location::Local("/d".into()));
        assert_eq!(name, raw.file_name().unwrap());
        assert_eq!(file_target(&h).unwrap().0, raw);
    }

    #[test]
    fn archive_hits_go_to_the_archive_but_cannot_be_viewed() {
        let h = Hit {
            location: Location::Archive {
                archive: "/d/x.zip".into(),
                inner: "docs/readme.txt".into(),
            },
            entry: Entry::new("readme.txt", EntryKind::File),
            content: None,
        };
        assert_eq!(folder_of(&h), "/d/x.zip#/docs");
        let (loc, name) = goto_target(&h).unwrap();
        assert_eq!(
            loc,
            Location::Archive {
                archive: "/d/x.zip".into(),
                inner: "docs".into()
            }
        );
        assert_eq!(name, "readme.txt");
        assert!(file_target(&h).is_none());
    }
}
