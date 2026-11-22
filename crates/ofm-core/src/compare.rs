//! Directory-compare engine for F27 (Shift+F2 "compare directories").
//!
//! Pure and UI-free: the input is two directory listings, the output is a
//! per-side set of marks (with a reason each, so the UI can offer "mark all
//! different" and "mark only newer") plus, for [`CompareMode::Content`], the
//! list of same-name/same-size pairs that a background job still has to
//! byte-compare. This module never touches the file system.
//!
//! Matching is done on a hash map of names: O(n) average, deterministic
//! output (marks are returned sorted by name, independent of input order).
//! Non-UTF-8 names are handled as raw bytes throughout.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::time::Duration;

use crate::entry::{Entry, EntryKind};

/// What kind of thing a compared entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link; compared by [`CompareEntry::link_target`] when both
    /// sides provide one, otherwise by name.
    Symlink,
    /// Device, fifo, socket, ...
    Other,
}

/// One entry handed to the compare engine.
///
/// Cheap to construct from a panel row via `From<&Entry>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareEntry {
    /// File name (bytes; must not contain a path separator).
    pub name: OsString,
    /// Kind.
    pub kind: CompareKind,
    /// Size in bytes (ignored for [`CompareKind::Dir`] pairs).
    pub size: u64,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    /// Symlink target as a byte string, when the provider knows it.
    /// `None` = compare the link by name only.
    pub link_target: Option<OsString>,
}

impl CompareEntry {
    /// Minimal entry (zero metadata) for tests and synthetic rows.
    pub fn new(name: impl Into<OsString>, kind: CompareKind) -> Self {
        CompareEntry {
            name: name.into(),
            kind,
            size: 0,
            mtime_ns: 0,
            link_target: None,
        }
    }
}

impl From<&Entry> for CompareEntry {
    fn from(e: &Entry) -> Self {
        let kind = match e.kind {
            EntryKind::File => CompareKind::File,
            EntryKind::Dir => CompareKind::Dir,
            EntryKind::Symlink { .. } => CompareKind::Symlink,
            EntryKind::Other => CompareKind::Other,
        };
        CompareEntry {
            name: e.name.clone(),
            kind,
            size: e.size,
            mtime_ns: e.mtime_ns,
            // `Entry` records the target *kind*, not the target string.
            link_target: None,
        }
    }
}

/// How strictly two entries with the same name are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareMode {
    /// Name presence only: an entry existing on both sides is never marked,
    /// regardless of type, size or mtime.
    NameOnly,
    /// Name + size (and type).
    NameSize,
    /// Name + size + mtime, with a configurable tolerance (default 2 s,
    /// covering FAT-style 2-second timestamp granularity).
    NameSizeMtime,
    /// Byte content: entries with the same name but different size are
    /// marked immediately; same-name, same-size files are returned as
    /// [`ContentCandidates`] for a background job to byte-compare, then
    /// resolved via [`apply_content_result`]. Mtime is ignored in this mode.
    Content,
}

/// Options for [`compare`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareOptions {
    /// Comparison mode (default [`CompareMode::NameSizeMtime`]).
    pub mode: CompareMode,
    /// Mtime fuzz for [`CompareMode::NameSizeMtime`]; differences up to this
    /// count as "same". Default 2 s.
    pub mtime_tolerance: Duration,
    /// Match names case-insensitively (Unicode lowercase for valid UTF-8,
    /// ASCII folding for non-UTF-8 byte names). Default false.
    pub case_insensitive: bool,
    /// When false, directories are invisible to the comparison entirely
    /// (filtered from both sides). Default true.
    pub include_dirs: bool,
    /// When true, a directory missing on the other side is *not* reported
    /// (directories still participate, so a file/dir name clash is still a
    /// [`MarkReason::TypeDiffers`]). Default false.
    pub ignore_dirs: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions {
            mode: CompareMode::NameSizeMtime,
            mtime_tolerance: Duration::from_secs(2),
            case_insensitive: false,
            include_dirs: true,
            ignore_dirs: false,
        }
    }
}

impl CompareOptions {
    /// Mtime tolerance in nanoseconds, saturated to `i128::MAX`.
    fn tolerance_ns(&self) -> i128 {
        i128::try_from(self.mtime_tolerance.as_nanos()).unwrap_or(i128::MAX)
    }

    /// True if `e` should not take part in matching at all.
    fn is_filtered(&self, e: &CompareEntry) -> bool {
        !self.include_dirs && e.kind == CompareKind::Dir
    }

    /// True if a missing mark may be reported for `e`.
    fn report_missing(&self, e: &CompareEntry) -> bool {
        !(self.ignore_dirs && e.kind == CompareKind::Dir)
    }
}

/// Why an entry was marked on one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkReason {
    /// No entry with this name on the other side.
    MissingOnOtherSide,
    /// Both sides have the name, but the kinds differ (file vs dir, ...).
    TypeDiffers,
    /// Both sides have the name, but the sizes differ.
    SizeDiffers,
    /// This side's entry is newer than the other side's (beyond tolerance).
    NewerHere,
    /// This side's entry is older than the other side's (beyond tolerance).
    OlderHere,
    /// Both sides are symlinks with different link targets.
    LinkTargetDiffers,
    /// A content comparison found the bytes differ ([`CompareMode::Content`]).
    ContentDiffers,
}

/// One marked entry in a [`CompareResult`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marked {
    /// Name as it appears on this side.
    pub name: OsString,
    /// Why it was marked.
    pub reason: MarkReason,
}

/// A same-name, same-size file pair awaiting a byte comparison by a
/// background job (see [`CompareMode::Content`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentPair {
    /// The matched name.
    pub name: OsString,
    /// Left-side entry.
    pub left: CompareEntry,
    /// Right-side entry.
    pub right: CompareEntry,
}

/// Same-name, same-size pairs still needing a byte comparison.
pub type ContentCandidates = Vec<ContentPair>;

/// Result of [`compare`]: per-side marks plus unresolved content candidates.
///
/// All mark vectors are sorted by name (bytes), so results are
/// deterministic regardless of input ordering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompareResult {
    /// Marks for the left listing, sorted by name.
    pub left_marks: Vec<Marked>,
    /// Marks for the right listing, sorted by name.
    pub right_marks: Vec<Marked>,
    /// Pairs a content job still has to compare (empty unless
    /// [`CompareMode::Content`]).
    pub content_candidates: ContentCandidates,
}

impl CompareResult {
    /// True when nothing differs (no marks; candidates may still be pending).
    pub fn no_marks(&self) -> bool {
        self.left_marks.is_empty() && self.right_marks.is_empty()
    }
}

/// Case-folded or raw name used as the matching key.
///
/// Valid UTF-8 gets full Unicode lowercase; non-UTF-8 names get ASCII
/// folding only, so distinct byte names never collide through lossy
/// conversion.
fn name_key<'a>(name: &'a OsStr, case_insensitive: bool) -> Cow<'a, [u8]> {
    let bytes = name.as_encoded_bytes();
    if !case_insensitive {
        return Cow::Borrowed(bytes);
    }
    match name.to_str() {
        Some(s) => Cow::Owned(s.to_lowercase().into_bytes()),
        None => Cow::Owned(bytes.iter().map(u8::to_ascii_lowercase).collect()),
    }
}

fn name_cmp(a: &OsStr, b: &[u8]) -> std::cmp::Ordering {
    a.as_encoded_bytes().cmp(b)
}

/// Insert `m` into `marks` (sorted by name, ascending).
fn insert_sorted(marks: &mut Vec<Marked>, m: Marked) {
    let pos = marks.partition_point(|x| name_cmp(&x.name, m.name.as_encoded_bytes()).is_lt());
    marks.insert(pos, m);
}

/// Compare two directory listings per `opts`.
///
/// Runs in O(n) average time (one hash pass per side). Entries with equal
/// names are paired greedily in input order when duplicates exist (e.g.
/// `FOO` and `foo` on one side with case-insensitive matching); each entry
/// is marked at most once.
pub fn compare(
    left: &[CompareEntry],
    right: &[CompareEntry],
    opts: &CompareOptions,
) -> CompareResult {
    let mut right_map: HashMap<Cow<'_, [u8]>, VecDeque<usize>> =
        HashMap::with_capacity(right.len());
    for (i, e) in right.iter().enumerate() {
        if opts.is_filtered(e) {
            continue;
        }
        right_map
            .entry(name_key(&e.name, opts.case_insensitive))
            .or_default()
            .push_back(i);
    }

    let mut result = CompareResult::default();
    let mut right_matched = vec![false; right.len()];

    for l in left {
        if opts.is_filtered(l) {
            continue;
        }
        let key = name_key(&l.name, opts.case_insensitive);
        let r_idx = right_map.get_mut(&key).and_then(VecDeque::pop_front);
        match r_idx {
            None => {
                if opts.report_missing(l) {
                    result.left_marks.push(Marked {
                        name: l.name.clone(),
                        reason: MarkReason::MissingOnOtherSide,
                    });
                }
            }
            Some(ri) => {
                right_matched[ri] = true;
                classify_pair(l, &right[ri], opts, &mut result);
            }
        }
    }

    for (ri, r) in right.iter().enumerate() {
        if right_matched[ri] || opts.is_filtered(r) {
            continue;
        }
        if opts.report_missing(r) {
            result.right_marks.push(Marked {
                name: r.name.clone(),
                reason: MarkReason::MissingOnOtherSide,
            });
        }
    }

    // Deterministic output independent of input ordering.
    result
        .left_marks
        .sort_by(|a, b| name_cmp(&a.name, b.name.as_encoded_bytes()));
    result
        .right_marks
        .sort_by(|a, b| name_cmp(&a.name, b.name.as_encoded_bytes()));
    result
}

/// Compare one matched pair and record marks/candidates in `result`.
fn classify_pair(
    l: &CompareEntry,
    r: &CompareEntry,
    opts: &CompareOptions,
    result: &mut CompareResult,
) {
    if l.kind != r.kind {
        let m = Marked {
            name: l.name.clone(),
            reason: MarkReason::TypeDiffers,
        };
        insert_sorted(&mut result.left_marks, m);
        let m = Marked {
            name: r.name.clone(),
            reason: MarkReason::TypeDiffers,
        };
        insert_sorted(&mut result.right_marks, m);
        return;
    }
    match l.kind {
        // Directories compare by name only.
        CompareKind::Dir => {}
        CompareKind::Symlink => {
            // By link target when both sides know it, else by name.
            if let (Some(a), Some(b)) = (&l.link_target, &r.link_target)
                && a != b
            {
                insert_sorted(
                    &mut result.left_marks,
                    Marked {
                        name: l.name.clone(),
                        reason: MarkReason::LinkTargetDiffers,
                    },
                );
                insert_sorted(
                    &mut result.right_marks,
                    Marked {
                        name: r.name.clone(),
                        reason: MarkReason::LinkTargetDiffers,
                    },
                );
            }
        }
        CompareKind::File | CompareKind::Other => match opts.mode {
            CompareMode::NameOnly => {}
            CompareMode::NameSize | CompareMode::NameSizeMtime => {
                if l.size != r.size {
                    mark_both(l, r, MarkReason::SizeDiffers, result);
                } else if opts.mode == CompareMode::NameSizeMtime {
                    let diff = l.mtime_ns.saturating_sub(r.mtime_ns);
                    let tol = opts.tolerance_ns();
                    // NewerHere / OlderHere are relative to the side being
                    // marked: the newer side gets NewerHere, the other
                    // OlderHere.
                    if diff > tol {
                        insert_sorted(
                            &mut result.left_marks,
                            Marked {
                                name: l.name.clone(),
                                reason: MarkReason::NewerHere,
                            },
                        );
                        insert_sorted(
                            &mut result.right_marks,
                            Marked {
                                name: r.name.clone(),
                                reason: MarkReason::OlderHere,
                            },
                        );
                    } else if diff < -tol {
                        insert_sorted(
                            &mut result.left_marks,
                            Marked {
                                name: l.name.clone(),
                                reason: MarkReason::OlderHere,
                            },
                        );
                        insert_sorted(
                            &mut result.right_marks,
                            Marked {
                                name: r.name.clone(),
                                reason: MarkReason::NewerHere,
                            },
                        );
                    }
                }
            }
            CompareMode::Content => {
                if l.size != r.size {
                    mark_both(l, r, MarkReason::SizeDiffers, result);
                } else {
                    result.content_candidates.push(ContentPair {
                        name: l.name.clone(),
                        left: l.clone(),
                        right: r.clone(),
                    });
                }
            }
        },
    }
}

/// Push `reason` for `l` and `r` onto the respective mark vectors.
fn mark_both(l: &CompareEntry, r: &CompareEntry, reason: MarkReason, result: &mut CompareResult) {
    insert_sorted(
        &mut result.left_marks,
        Marked {
            name: l.name.clone(),
            reason,
        },
    );
    insert_sorted(
        &mut result.right_marks,
        Marked {
            name: r.name.clone(),
            reason,
        },
    );
}

/// Record the outcome of a byte comparison for one [`ContentPair`].
///
/// `equal = true` marks nothing (the files match); `equal = false` marks
/// both sides with [`MarkReason::ContentDiffers`], keeping the mark vectors
/// sorted. The caller owns the candidate list and drops the pair from it.
pub fn apply_content_result(result: &mut CompareResult, pair: ContentPair, equal: bool) {
    if equal {
        return;
    }
    insert_sorted(
        &mut result.left_marks,
        Marked {
            name: pair.name.clone(),
            reason: MarkReason::ContentDiffers,
        },
    );
    insert_sorted(
        &mut result.right_marks,
        Marked {
            name: pair.name,
            reason: MarkReason::ContentDiffers,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::time::Instant;

    fn file(name: &str, size: u64, mtime_ns: i128) -> CompareEntry {
        CompareEntry {
            name: name.into(),
            kind: CompareKind::File,
            size,
            mtime_ns,
            link_target: None,
        }
    }

    fn names(marks: &[Marked]) -> Vec<OsString> {
        marks.iter().map(|m| m.name.clone()).collect()
    }

    #[test]
    fn identical_lists_are_unmarked() {
        let left = vec![
            file("a", 10, 100),
            file("b", 20, 200),
            CompareEntry::new("d", CompareKind::Dir),
        ];
        let right = left.clone();
        let r = compare(&left, &right, &CompareOptions::default());
        assert!(
            r.no_marks(),
            "left={:?} right={:?}",
            r.left_marks,
            r.right_marks
        );
        assert!(r.content_candidates.is_empty());
    }

    #[test]
    fn missing_on_both_sides() {
        let left = vec![file("only-left", 1, 0), file("both", 1, 0)];
        let right = vec![file("only-right", 1, 0), file("both", 1, 0)];
        let r = compare(&left, &right, &CompareOptions::default());
        assert_eq!(names(&r.left_marks), vec![OsString::from("only-left")]);
        assert_eq!(names(&r.right_marks), vec![OsString::from("only-right")]);
        assert!(
            r.left_marks
                .iter()
                .all(|m| m.reason == MarkReason::MissingOnOtherSide)
        );
    }

    #[test]
    fn size_differs_marks_both_sides() {
        let left = vec![file("x", 10, 0)];
        let right = vec![file("x", 11, 0)];
        let opts = CompareOptions {
            mode: CompareMode::NameSize,
            ..CompareOptions::default()
        };
        let r = compare(&left, &right, &opts);
        assert_eq!(r.left_marks.len(), 1);
        assert_eq!(r.right_marks.len(), 1);
        assert_eq!(r.left_marks[0].reason, MarkReason::SizeDiffers);
        assert_eq!(r.right_marks[0].reason, MarkReason::SizeDiffers);
    }

    #[test]
    fn name_only_ignores_everything_but_presence() {
        let left = vec![file("x", 10, 5)];
        let right = vec![file("x", 99, 9_999)];
        let opts = CompareOptions {
            mode: CompareMode::NameOnly,
            ..CompareOptions::default()
        };
        let r = compare(&left, &right, &opts);
        assert!(r.no_marks());
    }

    #[test]
    fn newer_older_beyond_tolerance() {
        // 3 s difference > default 2 s tolerance.
        let left = vec![file("x", 10, 3_000_000_000)];
        let right = vec![file("x", 10, 0)];
        let r = compare(&left, &right, &CompareOptions::default());
        assert_eq!(r.left_marks[0].reason, MarkReason::NewerHere);
        assert_eq!(r.right_marks[0].reason, MarkReason::OlderHere);
        // Swapped sides swap the reasons.
        let r = compare(&right, &left, &CompareOptions::default());
        assert_eq!(r.left_marks[0].reason, MarkReason::OlderHere);
        assert_eq!(r.right_marks[0].reason, MarkReason::NewerHere);
    }

    #[test]
    fn mtime_within_tolerance_is_unmarked() {
        let left = vec![file("x", 10, 2_000_000_000)];
        let right = vec![file("x", 10, 1)];
        let r = compare(&left, &right, &CompareOptions::default());
        assert!(r.no_marks());
    }

    #[test]
    fn mtime_exactly_at_tolerance_boundary() {
        // Diff == tolerance counts as same; diff == tolerance + 1 ns differs.
        let same_left = vec![file("x", 10, 2_000_000_000)];
        let same_right = vec![file("x", 10, 0)];
        let r = compare(&same_left, &same_right, &CompareOptions::default());
        assert!(r.no_marks());
        let diff_left = vec![file("x", 10, 2_000_000_001)];
        let r = compare(&diff_left, &same_right, &CompareOptions::default());
        assert_eq!(r.left_marks[0].reason, MarkReason::NewerHere);
    }

    #[test]
    fn type_differs_for_same_name() {
        let left = vec![CompareEntry::new("x", CompareKind::Dir)];
        let right = vec![file("x", 0, 0)];
        for mode in [
            CompareMode::NameOnly,
            CompareMode::NameSize,
            CompareMode::NameSizeMtime,
            CompareMode::Content,
        ] {
            let opts = CompareOptions {
                mode,
                ..CompareOptions::default()
            };
            let r = compare(&left, &right, &opts);
            assert_eq!(
                r.left_marks[0].reason,
                MarkReason::TypeDiffers,
                "mode {mode:?}"
            );
            assert_eq!(
                r.right_marks[0].reason,
                MarkReason::TypeDiffers,
                "mode {mode:?}"
            );
        }
    }

    #[test]
    fn case_insensitive_matching() {
        let left = vec![file("FOO", 1, 0), file("bar", 1, 0)];
        let right = vec![file("foo", 1, 0), file("BAR", 1, 0)];
        let opts = CompareOptions {
            case_insensitive: true,
            ..CompareOptions::default()
        };
        let r = compare(&left, &right, &opts);
        assert!(
            r.no_marks(),
            "left={:?} right={:?}",
            r.left_marks,
            r.right_marks
        );
        // Same input stays marked when case-sensitive.
        let r = compare(&left, &right, &CompareOptions::default());
        assert_eq!(r.left_marks.len(), 2);
        assert_eq!(r.right_marks.len(), 2);
    }

    #[test]
    fn case_insensitive_duplicates_are_deterministic() {
        // Both sides' "foo"/"FOO" collide on one key; pairing is greedy but
        // must not lose or duplicate entries.
        let left = vec![file("FOO", 1, 0), file("foo", 2, 0)];
        let right = vec![file("foo", 1, 0)];
        let opts = CompareOptions {
            case_insensitive: true,
            ..CompareOptions::default()
        };
        let r = compare(&left, &right, &opts);
        let total = r.left_marks.len() + r.right_marks.len();
        assert_eq!(
            total, 1,
            "left={:?} right={:?}",
            r.left_marks, r.right_marks
        );
    }

    #[test]
    fn symlinks_compared_by_target_when_known() {
        let mut l = CompareEntry::new("lnk", CompareKind::Symlink);
        l.link_target = Some(OsString::from("/a"));
        let mut same = CompareEntry::new("lnk", CompareKind::Symlink);
        same.link_target = Some(OsString::from("/a"));
        let mut other = CompareEntry::new("lnk", CompareKind::Symlink);
        other.link_target = Some(OsString::from("/b"));
        let opts = CompareOptions::default();
        let r = compare(std::slice::from_ref(&l), std::slice::from_ref(&same), &opts);
        assert!(r.no_marks());
        let r = compare(
            std::slice::from_ref(&l),
            std::slice::from_ref(&other),
            &opts,
        );
        assert_eq!(r.left_marks[0].reason, MarkReason::LinkTargetDiffers);
        assert_eq!(r.right_marks[0].reason, MarkReason::LinkTargetDiffers);
        // Unknown target: compared by name → equal.
        let r = compare(
            std::slice::from_ref(&l),
            std::slice::from_ref(&CompareEntry::new("lnk", CompareKind::Symlink)),
            &opts,
        );
        assert!(r.no_marks());
    }

    #[test]
    fn dirs_compare_by_name_only() {
        // Same name, wildly different sizes/mtimes → still equal.
        let mut l = CompareEntry::new("d", CompareKind::Dir);
        l.size = 100;
        l.mtime_ns = 5;
        let r_dir = CompareEntry::new("d", CompareKind::Dir);
        let r = compare(
            std::slice::from_ref(&l),
            std::slice::from_ref(&r_dir),
            &CompareOptions::default(),
        );
        assert!(r.no_marks());
    }

    #[test]
    fn include_dirs_false_hides_directories() {
        let left = vec![CompareEntry::new("d", CompareKind::Dir), file("f", 1, 0)];
        let right = vec![file("f", 1, 0)];
        let opts = CompareOptions {
            include_dirs: false,
            ..CompareOptions::default()
        };
        let r = compare(&left, &right, &opts);
        assert!(r.no_marks());
    }

    #[test]
    fn ignore_dirs_suppresses_only_missing_dir_marks() {
        // Missing dir not reported, but file/dir name clash still flagged.
        let left = vec![
            CompareEntry::new("d", CompareKind::Dir),
            CompareEntry::new("x", CompareKind::Dir),
        ];
        let right = vec![file("x", 1, 0)];
        let opts = CompareOptions {
            ignore_dirs: true,
            ..CompareOptions::default()
        };
        let r = compare(&left, &right, &opts);
        assert!(!r.no_marks());
        assert_eq!(r.left_marks.len(), 1);
        assert_eq!(r.left_marks[0].name, OsString::from("x"));
        assert_eq!(r.left_marks[0].reason, MarkReason::TypeDiffers);
    }

    #[test]
    fn content_mode_collects_candidates_and_applies_results() {
        let left = vec![
            file("same", 10, 1),
            file("diff-size", 10, 1),
            file("missing", 1, 0),
        ];
        let right = vec![file("same", 10, 9_999), file("diff-size", 11, 1)];
        let opts = CompareOptions {
            mode: CompareMode::Content,
            ..CompareOptions::default()
        };
        let mut r = compare(&left, &right, &opts);
        // diff-size flagged immediately (different size); same-name/same-size
        // becomes a candidate even though mtimes differ wildly.
        assert_eq!(r.content_candidates.len(), 1);
        assert_eq!(r.content_candidates[0].name, OsString::from("same"));
        assert_eq!(r.left_marks.len(), 2); // diff-size + missing
        assert_eq!(r.right_marks.len(), 1); // diff-size

        let Some(pair) = r.content_candidates.pop() else {
            panic!("one content candidate expected");
        };
        // Equal content → no new marks.
        apply_content_result(&mut r, pair.clone(), true);
        assert_eq!(r.left_marks.len(), 2);
        // Different content → both sides marked, kept sorted by name.
        apply_content_result(&mut r, pair, false);
        assert_eq!(r.left_marks.len(), 3);
        assert_eq!(r.right_marks.len(), 2);
        // Left marks sort as [diff-size, missing, same].
        assert_eq!(r.left_marks[2].name, OsString::from("same"));
        assert_eq!(r.left_marks[2].reason, MarkReason::ContentDiffers);
        assert_eq!(r.right_marks[1].name, OsString::from("same"));
        assert_eq!(r.right_marks[1].reason, MarkReason::ContentDiffers);
    }

    #[test]
    fn non_utf8_names_work() {
        let raw = |v: &[u8]| CompareEntry {
            name: OsString::from_vec(v.to_vec()),
            kind: CompareKind::File,
            size: 1,
            mtime_ns: 0,
            link_target: None,
        };
        let left = vec![raw(&[0x66, 0x80, 0x41]), raw(b"plain")];
        let right = vec![raw(&[0x66, 0x80, 0x61]), raw(b"plain")];
        // Case-sensitive: distinct byte names → both missing on each side.
        let r = compare(&left, &right, &CompareOptions::default());
        assert_eq!(r.left_marks.len(), 1);
        assert_eq!(r.right_marks.len(), 1);
        // Case-insensitive: ASCII 'A' folds to 'a', 0x80 stays → match.
        let opts = CompareOptions {
            case_insensitive: true,
            ..CompareOptions::default()
        };
        let r = compare(&left, &right, &opts);
        assert!(
            r.no_marks(),
            "left={:?} right={:?}",
            r.left_marks,
            r.right_marks
        );
    }

    #[test]
    fn marks_sorted_regardless_of_input_order() {
        let left = vec![file("z", 1, 0), file("m", 1, 0), file("a", 1, 0)];
        let right: Vec<CompareEntry> = Vec::new();
        let r = compare(&left, &right, &CompareOptions::default());
        assert_eq!(
            names(&r.left_marks),
            vec![
                OsString::from("a"),
                OsString::from("m"),
                OsString::from("z")
            ]
        );
    }

    #[test]
    fn entry_conversion_maps_kinds() {
        let e = Entry::new("x", EntryKind::Symlink { target: None });
        let c = CompareEntry::from(&e);
        assert_eq!(c.kind, CompareKind::Symlink);
        assert_eq!(c.name, OsString::from("x"));
    }

    #[test]
    fn handles_100k_entries_fast() {
        let mk = |n: u32, size: u64| file(&format!("f{n:07}"), size, i128::from(n));
        let left: Vec<CompareEntry> = (0..100_000).map(|n| mk(n, 100)).collect();
        // Same listing with every 1000th entry changed or dropped.
        let right: Vec<CompareEntry> = (0..100_000)
            .filter(|n| n % 1000 != 999)
            .map(|n| mk(n, if n % 1000 == 500 { 101 } else { 100 }))
            .collect();
        let opts = CompareOptions::default();
        let start = Instant::now();
        let r = compare(&left, &right, &opts);
        let elapsed = start.elapsed();
        // Generous on purpose: this guards against an O(n^2) regression (minutes at 100k
        // entries), not against a loaded CI box in a debug build (flaked at 1 s).
        assert!(elapsed.as_secs() < 15, "compare took {elapsed:?}");
        // Left: 100 size-changed + 100 names the right side dropped.
        assert_eq!(r.left_marks.len(), 200, "left marks changed?");
        // Right: only the 100 size-changed entries carry a mark (the dropped
        // names exist on the left, so there is nothing to mark on the right).
        assert_eq!(r.right_marks.len(), 100);
        assert!(
            r.right_marks
                .iter()
                .all(|m| m.reason == MarkReason::SizeDiffers)
        );
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::collection::vec;
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn arb_kind() -> impl Strategy<Value = CompareKind> {
        (0..4u8).prop_map(|k| match k {
            0 => CompareKind::File,
            1 => CompareKind::Dir,
            2 => CompareKind::Symlink,
            _ => CompareKind::Other,
        })
    }

    fn arb_entry() -> impl Strategy<Value = CompareEntry> {
        (
            "[a-zA-Z0-9._-]{0,24}",
            arb_kind(),
            0u64..10_000,
            0i128..1_000_000_000_000,
        )
            .prop_map(|(name, kind, size, mtime_ns)| CompareEntry {
                name: name.into(),
                kind,
                size,
                mtime_ns,
                link_target: None,
            })
    }

    /// Entries with unique names (duplicates within one side would be paired
    /// greedily, which set-based properties below do not model).
    fn arb_entries_unique() -> impl Strategy<Value = Vec<CompareEntry>> {
        vec(arb_entry(), 0..64).prop_map(|entries| {
            let mut seen = HashSet::new();
            entries
                .into_iter()
                .filter(|e| seen.insert(e.name.as_encoded_bytes().to_vec()))
                .collect()
        })
    }

    fn name_set(entries: &[CompareEntry]) -> HashSet<Vec<u8>> {
        entries
            .iter()
            .map(|e| e.name.as_encoded_bytes().to_vec())
            .collect()
    }

    fn mark_names(marks: &[Marked], reason: MarkReason) -> HashSet<Vec<u8>> {
        marks
            .iter()
            .filter(|m| m.reason == reason)
            .map(|m| m.name.as_encoded_bytes().to_vec())
            .collect()
    }

    proptest! {
        /// Comparing a listing with itself never marks anything.
        #[test]
        fn compare_with_itself_marks_nothing(entries in vec(arb_entry(), 0..64)) {
            let opts = CompareOptions::default();
            let r = compare(&entries, &entries, &opts);
            prop_assert!(r.left_marks.is_empty(), "left: {:?}", r.left_marks);
            prop_assert!(r.right_marks.is_empty(), "right: {:?}", r.right_marks);
        }

        /// Marks are always a subset of the input names on each side.
        #[test]
        fn marks_are_subsets_of_input_names(
            left in arb_entries_unique(),
            right in arb_entries_unique(),
        ) {
            let r = compare(&left, &right, &CompareOptions::default());
            let left_names = name_set(&left);
            let right_names = name_set(&right);
            for m in &r.left_marks {
                prop_assert!(left_names.contains(m.name.as_encoded_bytes()),
                    "left mark {:?} not in left names", m.name);
            }
            for m in &r.right_marks {
                prop_assert!(right_names.contains(m.name.as_encoded_bytes()),
                    "right mark {:?} not in right names", m.name);
            }
        }

        /// Symmetry: names marked missing on the left are exactly the names
        /// that exist only on the right, and vice versa.
        #[test]
        fn missing_marks_are_symmetric(
            left in arb_entries_unique(),
            right in arb_entries_unique(),
        ) {
            let r = compare(&left, &right, &CompareOptions::default());
            let left_only = &name_set(&left) - &name_set(&right);
            let right_only = &name_set(&right) - &name_set(&left);
            prop_assert_eq!(mark_names(&r.left_marks, MarkReason::MissingOnOtherSide), left_only);
            prop_assert_eq!(mark_names(&r.right_marks, MarkReason::MissingOnOtherSide), right_only);
        }

        /// With zero mtime tolerance, every NewerHere on the left has a
        /// matching OlderHere on the right for the same name (and vice versa).
        #[test]
        fn newer_here_implies_older_there(
            left in arb_entries_unique(),
            right in arb_entries_unique(),
        ) {
            let opts = CompareOptions {
                mtime_tolerance: Duration::ZERO,
                ..CompareOptions::default()
            };
            let r = compare(&left, &right, &opts);
            let left_newer = mark_names(&r.left_marks, MarkReason::NewerHere);
            let right_older = mark_names(&r.right_marks, MarkReason::OlderHere);
            let left_older = mark_names(&r.left_marks, MarkReason::OlderHere);
            let right_newer = mark_names(&r.right_marks, MarkReason::NewerHere);
            prop_assert_eq!(left_newer, right_older);
            prop_assert_eq!(left_older, right_newer);
        }
    }
}
