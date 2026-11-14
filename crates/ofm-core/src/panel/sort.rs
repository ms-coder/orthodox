//! Sorting: natural order, keys, spec.

use std::cmp::Ordering;
use std::ffi::OsStr;

use crate::entry::Entry;

/// Column to sort by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortKey {
    /// Natural name order (`file2` < `file10`).
    #[default]
    Name,
    /// Extension, then name.
    Ext,
    /// Size, then name.
    Size,
    /// Modification time, then name.
    Mtime,
}

/// Sort configuration of a panel. `..` is always first regardless of this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SortSpec {
    /// Key.
    pub key: SortKey,
    /// Descending order (reverses the key order; directories still come first).
    pub reverse: bool,
    /// Directories before files.
    pub dirs_first: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        SortSpec {
            key: SortKey::Name,
            reverse: false,
            dirs_first: true,
        }
    }
}

/// Natural, case-insensitive-first comparison of file names (digit runs compare numerically;
/// ties broken by case-sensitive byte order so the result is a total order). Works on bytes.
///
/// Case-insensitivity is ASCII-only in v1.0; non-ASCII bytes compare by value.
pub fn natural_cmp(a: &OsStr, b: &OsStr) -> Ordering {
    natural_cmp_bytes(a.as_encoded_bytes(), b.as_encoded_bytes())
}

/// `natural_cmp` on raw byte slices (extensions are compared without rebuilding an `OsStr`).
/// Visible to sibling modules in `panel` (the model's arena comparator uses it).
///
/// Each string is treated as a stream of independent tokens — single ASCII-lowercased bytes, or
/// whole digit runs compared numerically (leading zeros ignored). Tokenization must not depend on
/// the *other* string: a joint walk (consume a digit run only when both sides are on digits) breaks
/// transitivity, e.g. `"a" < "a0" < "A1"` but `"a" > "A1"`. A digit run always compares against a
/// non-digit byte by its first digit byte; since the digit range `0x30..=0x39` contains no other
/// class member, that is class-consistent and matches ASCII order. Equal token streams fall back to
/// raw byte order (case, digit padding), so distinct names never compare `Equal`.
pub(super) fn natural_cmp_bytes(a: &[u8], b: &[u8]) -> Ordering {
    // Fast path: skip the common prefix (memcmp speed), then back up so the token logic starts
    // on a digit-run boundary for both strings (a run may straddle the divergence point).
    let mut i = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    while i > 0 && a[i - 1].is_ascii_digit() {
        i -= 1;
    }
    let mut j = i;
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        match (x.is_ascii_digit(), y.is_ascii_digit()) {
            (true, true) => {
                let end_a = digit_run_end(a, i);
                let end_b = digit_run_end(b, j);
                match cmp_numeric(&a[i..end_a], &b[j..end_b]) {
                    Ordering::Equal => {
                        i = end_a;
                        j = end_b;
                    }
                    ord => return ord,
                }
            }
            (true, false) => return x.cmp(&y.to_ascii_lowercase()),
            (false, true) => return x.to_ascii_lowercase().cmp(&y),
            (false, false) => {
                let (lx, ly) = (x.to_ascii_lowercase(), y.to_ascii_lowercase());
                if lx != ly {
                    return lx.cmp(&ly);
                }
                i += 1;
                j += 1;
            }
        }
    }
    match (i < a.len(), j < b.len()) {
        (true, _) => Ordering::Greater,  // b is a token-prefix of a
        (false, true) => Ordering::Less, // a is a token-prefix of b
        (false, false) => a.cmp(b),      // equal up to case / digit padding
    }
}

/// Index just past the run of ASCII digits starting at `i`.
fn digit_run_end(s: &[u8], i: usize) -> usize {
    let mut e = i;
    while e < s.len() && s[e].is_ascii_digit() {
        e += 1;
    }
    e
}

/// Compare two ASCII digit runs numerically (leading zeros ignored, `007` == `7`).
fn cmp_numeric(x: &[u8], y: &[u8]) -> Ordering {
    let xs = strip_zeros(x);
    let ys = strip_zeros(y);
    xs.len().cmp(&ys.len()).then_with(|| xs.cmp(ys))
}

fn strip_zeros(s: &[u8]) -> &[u8] {
    let first_nonzero = s.iter().position(|&c| c != b'0');
    match first_nonzero {
        Some(p) => &s[p..],
        // All zeros (including the empty run): canonicalize to a single zero.
        None => &s[s.len().saturating_sub(1)..],
    }
}

/// Extension of a file name in bytes: everything after the last `.` when that `.` is not the
/// first byte (`.bashrc` has no extension). Empty for names without a dot.
fn name_ext(name: &OsStr) -> &[u8] {
    ext_bytes(name.as_encoded_bytes())
}

/// [`name_ext`](super::sort::name_ext) on raw name bytes.
pub(super) fn ext_bytes(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&c| c == b'.') {
        Some(p) if p > 0 => &name[p + 1..],
        _ => b"",
    }
}

/// Compare two entries by `spec` (a total order: distinct names never compare equal).
/// The synthetic `..` row is *not* handled here — the model pins it first.
pub fn cmp_entries(spec: &SortSpec, a: &Entry, b: &Entry) -> Ordering {
    if spec.dirs_first && a.kind.is_dir_like() != b.kind.is_dir_like() {
        // Directories first regardless of `reverse`.
        return if a.kind.is_dir_like() {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    let key = match spec.key {
        SortKey::Name => natural_cmp(&a.name, &b.name),
        SortKey::Ext => name_ext_cmp(&a.name, &b.name).then_with(|| natural_cmp(&a.name, &b.name)),
        SortKey::Size => a
            .size
            .cmp(&b.size)
            .then_with(|| natural_cmp(&a.name, &b.name)),
        SortKey::Mtime => a
            .mtime_ns
            .cmp(&b.mtime_ns)
            .then_with(|| natural_cmp(&a.name, &b.name)),
    };
    if spec.reverse { key.reverse() } else { key }
}

fn name_ext_cmp(a: &OsStr, b: &OsStr) -> Ordering {
    natural_cmp_bytes(name_ext(a), name_ext(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;
    use std::ffi::OsStr;

    fn n(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    #[test]
    fn natural_numeric_runs() {
        assert_eq!(natural_cmp(n("file2"), n("file10")), Ordering::Less);
        assert_eq!(natural_cmp(n("file10"), n("file2")), Ordering::Greater);
        assert_eq!(natural_cmp(n("a9b1"), n("a9b10")), Ordering::Less);
        assert_eq!(natural_cmp(n("2"), n("10")), Ordering::Less);
        assert_eq!(natural_cmp(n("0x10"), n("0x9")), Ordering::Greater); // 10 > 9
    }

    #[test]
    fn natural_case_insensitive_first() {
        assert_eq!(natural_cmp(n("B"), n("a")), Ordering::Greater); // letters before case
        assert_eq!(natural_cmp(n("File"), n("file2")), Ordering::Less); // equal stem, digit later
        // Ties broken by case-sensitive byte order (distinct names never compare Equal).
        assert_eq!(natural_cmp(n("A"), n("a")), Ordering::Less);
        assert_eq!(natural_cmp(n("a"), n("A")), Ordering::Greater);
        assert_eq!(natural_cmp(n("FILE"), n("file")), Ordering::Less);
        // Numerically equal digit runs with different padding: byte order decides.
        assert_eq!(natural_cmp(n("file02"), n("file2")), Ordering::Less); // '0' < '2'
        assert_eq!(natural_cmp(n("file2"), n("file02")), Ordering::Greater);
        assert_eq!(natural_cmp(n("a0"), n("a00")), Ordering::Less);
    }

    #[test]
    fn natural_prefixes_and_empty() {
        assert_eq!(natural_cmp(n(""), n("")), Ordering::Equal);
        assert_eq!(natural_cmp(n(""), n("a")), Ordering::Less);
        assert_eq!(natural_cmp(n("ab"), n("abc")), Ordering::Less);
        assert_eq!(natural_cmp(n("abc"), n("ab")), Ordering::Greater);
    }

    #[test]
    fn natural_is_antisymmetric() {
        let names = [
            "", "a", "A", "ab", "aB", "a2", "a10", "a02", "a", "z", "Z", "0", "00",
        ];
        for x in names {
            for y in names {
                assert_eq!(
                    natural_cmp(n(x), n(y)).reverse(),
                    natural_cmp(n(y), n(x)),
                    "{x:?} vs {y:?}"
                );
            }
        }
    }

    /// Exhaustive transitivity check over all strings of a small alphabet (digits + both cases):
    /// a comparator fed to `sort_by` must be a total order or sorting may panic / misorder.
    #[test]
    fn natural_is_transitive_exhaustive() {
        let alphabet: &[u8] = b"01aA";
        let mut strings: Vec<Vec<u8>> = Vec::new();
        for len in 0..=3usize {
            let mut cur = Vec::new();
            grow(alphabet, len, &mut cur, &mut strings);
        }
        let refs: Vec<&[u8]> = strings.iter().map(|s| s.as_slice()).collect();
        for x in &refs {
            for y in &refs {
                if natural_cmp_bytes(x, y) != Ordering::Less {
                    continue;
                }
                for z in &refs {
                    if natural_cmp_bytes(y, z) == Ordering::Less {
                        assert_eq!(
                            natural_cmp_bytes(x, z),
                            Ordering::Less,
                            "{:?} < {:?} < {:?} is inconsistent",
                            String::from_utf8_lossy(x),
                            String::from_utf8_lossy(y),
                            String::from_utf8_lossy(z)
                        );
                    }
                }
            }
        }
    }

    fn grow(alphabet: &[u8], len: usize, cur: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
        if cur.len() == len {
            out.push(cur.clone());
            return;
        }
        for &c in alphabet {
            cur.push(c);
            grow(alphabet, len, cur, out);
            cur.pop();
        }
    }

    #[test]
    fn ext_extraction() {
        assert_eq!(name_ext(n("a.txt")), b"txt");
        assert_eq!(name_ext(n("a.tar.gz")), b"gz");
        assert_eq!(name_ext(n("archive.tar.gz")), b"gz");
        assert_eq!(name_ext(n(".bashrc")), b"");
        assert_eq!(name_ext(n("noext")), b"");
        assert_eq!(name_ext(n("a.")), b"");
    }

    fn e(name: &str, kind: EntryKind, size: u64, mtime: i128) -> Entry {
        let mut e = Entry::new(name, kind);
        e.size = size;
        e.mtime_ns = mtime;
        e
    }

    #[test]
    fn cmp_entries_keys() {
        let f2 = e("file2.txt", EntryKind::File, 10, 5);
        let f10 = e("file10.txt", EntryKind::File, 20, 5);
        let mut spec = SortSpec {
            key: SortKey::Name,
            reverse: false,
            dirs_first: false,
        };
        assert_eq!(cmp_entries(&spec, &f2, &f10), Ordering::Less);
        spec.key = SortKey::Size;
        assert_eq!(cmp_entries(&spec, &f2, &f10), Ordering::Less); // 10 < 20
        spec.key = SortKey::Mtime;
        assert_eq!(cmp_entries(&spec, &f2, &f10), Ordering::Less); // mtime ties -> name
        spec.key = SortKey::Ext;
        assert_eq!(cmp_entries(&spec, &f2, &f10), Ordering::Less); // same ext -> name
        spec.reverse = true;
        spec.key = SortKey::Size;
        assert_eq!(cmp_entries(&spec, &f2, &f10), Ordering::Greater);
    }

    #[test]
    fn cmp_entries_dirs_first_beats_reverse() {
        let dir = e("zzz", EntryKind::Dir, 0, 0);
        let file = e("aaa", EntryKind::File, 0, 0);
        let link_dir = e(
            "mmm",
            EntryKind::Symlink {
                target: Some(crate::entry::TargetKind::Dir),
            },
            0,
            0,
        );
        for reverse in [false, true] {
            let spec = SortSpec {
                key: SortKey::Name,
                reverse,
                dirs_first: true,
            };
            assert_eq!(cmp_entries(&spec, &dir, &file), Ordering::Less);
            assert_eq!(cmp_entries(&spec, &link_dir, &file), Ordering::Less);
        }
        let spec = SortSpec {
            key: SortKey::Name,
            reverse: false,
            dirs_first: false,
        };
        assert_eq!(cmp_entries(&spec, &dir, &file), Ordering::Greater); // zzz > aaa by name
    }
}
