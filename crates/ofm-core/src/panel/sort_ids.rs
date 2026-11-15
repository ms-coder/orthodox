//! Fast bulk sorting of entry ids using precomputed key prefixes.
//!
//! Sorting 100k entries through [`cmp_entries`](super::sort::cmp_entries) is dominated by cache
//! misses (entry -> name heap pointer). Here every item carries 8-byte order-preserving key
//! prefixes, so most comparisons never touch the entries. The resulting order is identical to
//! `cmp_entries` (checked by a test); ties on a prefix fall back to the full comparison.

use std::cmp::Ordering;

use super::sort::{SortKey, SortSpec, ext_bytes, natural_cmp_bytes};
use crate::entry::Entry;

/// Width of a key prefix in bytes.
const KEY_LEN: usize = 16;

/// Sort key prefix of a name or extension: the first 16 bytes of a collation encoding, big-endian,
/// such that `a < b` as numbers implies `natural_cmp_bytes(a, b) == Less`. Equal prefixes decide
/// nothing (the caller falls back to the full comparison).
///
/// Encoding: non-numeric bytes are ASCII-lowercased; a run of ASCII numerals becomes `0x30`, a
/// length byte and the numerals without leading zeros (so runs compare numerically). A numeral
/// versus a non-numeral byte is decided by the byte value in the natural order, and `0x30` sits on
/// the same side of every non-numeral byte as any numeral does. Runs of 255 or more numerals stop
/// the prefix after the length byte. The result is zero padded; a name that is a token-prefix of
/// another encodes shorter and so sorts first, as in the natural order (names never contain NUL).
fn text_key(bytes: &[u8]) -> u128 {
    text_key_exact(bytes).0
}

/// [`text_key`] plus whether the key is *exact*: it encodes the whole input injectively (nothing
/// cut off, no upper-case letters, no padded zeros), so equal exact keys mean equal bytes.
fn text_key_exact(bytes: &[u8]) -> (u128, bool) {
    let mut k = [0u8; KEY_LEN];
    let mut n = 0;
    let mut i = 0;
    let mut exact = true;
    while i < bytes.len() && n < KEY_LEN {
        let c = bytes[i];
        if !c.is_ascii_digit() {
            exact &= !c.is_ascii_uppercase();
            k[n] = c.to_ascii_lowercase();
            n += 1;
            i += 1;
            continue;
        }
        let end = bytes[i..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map_or(bytes.len(), |p| i + p);
        let run = &bytes[i..end];
        let first = run.iter().position(|&d| d != b'0').unwrap_or(run.len() - 1);
        let nums = &run[first..];
        exact &= first == 0;
        k[n] = b'0';
        n += 1;
        if n < KEY_LEN {
            k[n] = u8::try_from(nums.len()).unwrap_or(u8::MAX);
            n += 1;
        }
        if nums.len() >= usize::from(u8::MAX) {
            exact = false;
            break;
        }
        let before = n;
        for &d in nums {
            if n == KEY_LEN {
                break;
            }
            k[n] = d;
            n += 1;
        }
        exact &= n - before == nums.len();
        i = end;
    }
    // Anything left over (cut off at the key width, or after a capped run) is not encoded.
    exact &= i == bytes.len();
    (u128::from_be_bytes(k), exact)
}

/// Monotonic (non-strict) map of a modification time to a `u64`.
fn mtime_key(ns: i128) -> u64 {
    let clamped = ns.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
    (clamped as u64) ^ (1 << 63)
}

/// One name-sort row (32 bytes). Directories are separated before sorting, so no kind field.
struct Item {
    id: u32,
    /// Name key prefix.
    key: u128,
}

fn cmp_items(spec: &SortSpec, entries: &[Entry], a: &Item, b: &Item) -> Ordering {
    let key = a.key.cmp(&b.key).then_with(|| {
        let (ea, eb) = (&entries[a.id as usize], &entries[b.id as usize]);
        natural_cmp_bytes(ea.name.as_encoded_bytes(), eb.name.as_encoded_bytes())
    });
    if spec.reverse { key.reverse() } else { key }
}

/// Name sort through key prefixes.
fn sort_group_name(spec: &SortSpec, entries: &[Entry], ids: &mut [u32]) {
    let mut items: Vec<Item> = ids
        .iter()
        .map(|&id| Item {
            id,
            key: text_key(entries[id as usize].name.as_encoded_bytes()),
        })
        .collect();
    items.sort_unstable_by(|a, b| cmp_items(spec, entries, a, b));
    for (slot, item) in ids.iter_mut().zip(&items) {
        *slot = item.id;
    }
}

/// A row of the extension sort: the name key plus the extension key, computed in input order so
/// the name pointer is not chased again after the name sort scrambled the ids.
struct ExtItem {
    id: u32,
    exact: bool,
    ext: u64,
    name: u128,
}

/// Extension sort: name order first, then an extension-key sort that breaks ties by the name rank,
/// so equal extensions keep their name order (reversed together with the extension when `reverse`).
fn sort_group_ext(spec: &SortSpec, entries: &[Entry], ids: &mut [u32]) {
    let mut items: Vec<ExtItem> = ids
        .iter()
        .map(|&id| {
            let name = entries[id as usize].name.as_encoded_bytes();
            let (k, exact) = text_key_exact(ext_bytes(name));
            ExtItem {
                id,
                // Only the top 8 bytes of the key are kept: it is exact for the stored `u64` only
                // if nothing was encoded below them (else two extensions that differ after byte 8
                // would compare equal, e.g. "0__2" vs "0__2_").
                exact: exact && k & u128::from(u64::MAX) == 0,
                ext: (k >> 64) as u64,
                name: text_key(name),
            }
        })
        .collect();
    items.sort_unstable_by(|a, b| {
        let key = a.name.cmp(&b.name).then_with(|| {
            let (ea, eb) = (&entries[a.id as usize], &entries[b.id as usize]);
            natural_cmp_bytes(ea.name.as_encoded_bytes(), eb.name.as_encoded_bytes())
        });
        if spec.reverse { key.reverse() } else { key }
    });
    // `items` is in name order: the position is the name rank.
    if items.iter().all(|it| it.exact) {
        // Common case: every extension key is exact, so (key, rank) packed in a `u128` is the
        // whole order. For `reverse` the key is complemented; the rank stays ascending.
        let mut packed: Vec<u128> = items
            .iter()
            .enumerate()
            .map(|(rank, it)| {
                let k = if spec.reverse { !it.ext } else { it.ext };
                (u128::from(k) << 64) | rank as u128
            })
            .collect();
        packed.sort_unstable();
        for (slot, p) in ids.iter_mut().zip(&packed) {
            *slot = items[(*p as u32) as usize].id;
        }
        return;
    }
    // Some extension is longer than the key: compare those fully, ties by rank.
    let mut rows: Vec<(usize, &ExtItem)> = items.iter().enumerate().collect();
    rows.sort_unstable_by(|(ra, a), (rb, b)| {
        let ext = a.ext.cmp(&b.ext).then_with(|| {
            if a.exact && b.exact {
                return Ordering::Equal;
            }
            natural_cmp_bytes(
                ext_bytes(entries[a.id as usize].name.as_encoded_bytes()),
                ext_bytes(entries[b.id as usize].name.as_encoded_bytes()),
            )
        });
        (if spec.reverse { ext.reverse() } else { ext }).then(ra.cmp(rb))
    });
    for (slot, (_, it)) in ids.iter_mut().zip(&rows) {
        *slot = it.id;
    }
}

/// Size and time sorts: compact `(key, id)` pairs sort much faster than the text items.
fn sort_group_numeric(spec: &SortSpec, entries: &[Entry], ids: &mut [u32]) {
    let key_of = |e: &Entry| match spec.key {
        SortKey::Mtime => mtime_key(e.mtime_ns),
        _ => e.size,
    };
    let mut pairs: Vec<(u64, u32)> = ids
        .iter()
        .map(|&id| (key_of(&entries[id as usize]), id))
        .collect();
    pairs.sort_unstable_by(|a, b| {
        let key = a.0.cmp(&b.0).then_with(|| {
            let (ea, eb) = (&entries[a.1 as usize], &entries[b.1 as usize]);
            // The clamped time key may tie for distinct times: compare the real ones.
            let time = if spec.key == SortKey::Mtime {
                ea.mtime_ns.cmp(&eb.mtime_ns)
            } else {
                Ordering::Equal
            };
            time.then_with(|| {
                natural_cmp_bytes(ea.name.as_encoded_bytes(), eb.name.as_encoded_bytes())
            })
        });
        if spec.reverse { key.reverse() } else { key }
    });
    for (slot, pair) in ids.iter_mut().zip(&pairs) {
        *slot = pair.1;
    }
}

/// Sort one group of ids (all directories, all files, or everything) by the spec's key.
fn sort_group(spec: &SortSpec, entries: &[Entry], ids: &mut [u32]) {
    match spec.key {
        SortKey::Name => sort_group_name(spec, entries, ids),
        SortKey::Ext => sort_group_ext(spec, entries, ids),
        SortKey::Size | SortKey::Mtime => sort_group_numeric(spec, entries, ids),
    }
}

/// Sort `ids` (indices into `entries`) in place by `spec`; same order as `cmp_entries`
/// (with `dirs_first`, directories come first regardless of `reverse`).
pub(super) fn sort_ids(spec: &SortSpec, entries: &[Entry], ids: &mut [u32]) {
    if !spec.dirs_first {
        sort_group(spec, entries, ids);
        return;
    }
    let (mut dirs, mut files): (Vec<u32>, Vec<u32>) = ids
        .iter()
        .partition(|&&id| entries[id as usize].kind.is_dir_like());
    sort_group(spec, entries, &mut dirs);
    sort_group(spec, entries, &mut files);
    for (slot, id) in ids.iter_mut().zip(dirs.into_iter().chain(files)) {
        *slot = id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;
    use crate::panel::sort::cmp_entries;
    use proptest::prelude::*;

    fn entry(name: &str, dir: bool, size: u64, mtime: i128) -> Entry {
        let kind = if dir { EntryKind::Dir } else { EntryKind::File };
        let mut e = Entry::new(name, kind);
        e.size = size;
        e.mtime_ns = mtime;
        e
    }

    fn reference_sort(spec: &SortSpec, entries: &[Entry]) -> Vec<u32> {
        let mut ids: Vec<u32> = (0..entries.len() as u32).collect();
        ids.sort_by(|&a, &b| cmp_entries(spec, &entries[a as usize], &entries[b as usize]));
        ids
    }

    #[test]
    fn ext_sort_tells_apart_extensions_that_differ_after_the_stored_key_width() {
        // Found by proptest: "A.0__2" and "_.0__2_" have extensions "0__2" / "0__2_" whose encoded
        // keys agree in the first 8 bytes (all that the extension sort stores) and differ in the 9th.
        let entries = vec![entry("_.0__2_", false, 0, 0), entry("A.0__2", false, 0, 0)];
        for reverse in [false, true] {
            let spec = SortSpec {
                key: SortKey::Ext,
                reverse,
                dirs_first: false,
            };
            let mut fast: Vec<u32> = (0..entries.len() as u32).collect();
            sort_ids(&spec, &entries, &mut fast);
            assert_eq!(fast, reference_sort(&spec, &entries), "reverse={reverse}");
        }
    }

    proptest! {
        /// The keyed bulk sort must equal sorting with the reference comparator.
        #[test]
        fn matches_cmp_entries(
            raw in proptest::collection::vec(
                (r"[a-cA-C0-2._]{0,24}", any::<bool>(), 0u64..4, -3i128..3), 0..60),
            key in 0usize..4,
            reverse in any::<bool>(),
            dirs_first in any::<bool>(),
        ) {
            let mut seen = std::collections::HashSet::new();
            let entries: Vec<Entry> = raw
                .into_iter()
                .filter(|(n, ..)| seen.insert(n.clone()))
                .map(|(n, d, s, m)| entry(&n, d, s, m))
                .collect();
            let key = [SortKey::Name, SortKey::Ext, SortKey::Size, SortKey::Mtime][key];
            let spec = SortSpec { key, reverse, dirs_first };
            let mut fast: Vec<u32> = (0..entries.len() as u32).collect();
            sort_ids(&spec, &entries, &mut fast);
            prop_assert_eq!(fast, reference_sort(&spec, &entries));
        }
    }

    proptest! {
        /// A strictly smaller prefix key must agree with the natural order.
        #[test]
        fn key_order_implies_natural_order(
            a in r"[a-cA-C0-3._]{0,24}", b in r"[a-cA-C0-3._]{0,24}"
        ) {
            let (ka, kb) = (text_key(a.as_bytes()), text_key(b.as_bytes()));
            if ka != kb {
                prop_assert_eq!(ka.cmp(&kb), natural_cmp_bytes(a.as_bytes(), b.as_bytes()));
            }
        }
    }

    #[test]
    fn long_common_prefix_and_digits() {
        let names = [
            "abcdefgh1",
            "abcdefgh10",
            "abcdefgh9",
            "abcdefghZ",
            "abcdefgh",
            "a1",
            "a10",
            "a9",
        ];
        let entries: Vec<Entry> = names.iter().map(|n| entry(n, false, 0, 0)).collect();
        let spec = SortSpec::default();
        let mut fast: Vec<u32> = (0..entries.len() as u32).collect();
        sort_ids(&spec, &entries, &mut fast);
        assert_eq!(fast, reference_sort(&spec, &entries));
    }
}
