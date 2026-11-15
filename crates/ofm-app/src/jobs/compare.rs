//! Byte-for-byte comparison of two files for the conflict dialog's "Compare" (F16). Runs on a
//! worker thread through the `Fs` seam of `ofm-jobs` (never on the GTK thread).

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use ofm_jobs::{Fs, FsFile};

/// Result of [`compare_files`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// Same length and same bytes.
    Identical,
    /// First differing byte offset (a shorter file differs at its length).
    DiffersAt(u64),
    /// Stopped because `cancel` was set.
    Cancelled,
}

const CHUNK: usize = 256 * 1024;

/// Read until `buf` is full or EOF; returns the number of bytes read.
fn fill(f: &mut dyn FsFile, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Compare the contents of `a` and `b` (symlinks are not followed).
pub fn compare_files(
    fs: &dyn Fs,
    a: &Path,
    b: &Path,
    cancel: &AtomicBool,
) -> io::Result<Comparison> {
    let mut fa = fs.open_read(a)?;
    let mut fb = fs.open_read(b)?;
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let mut offset = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Comparison::Cancelled);
        }
        let na = fill(fa.as_mut(), &mut ba)?;
        let nb = fill(fb.as_mut(), &mut bb)?;
        let common = na.min(nb);
        if let Some(i) = ba[..common]
            .iter()
            .zip(&bb[..common])
            .position(|(x, y)| x != y)
        {
            return Ok(Comparison::DiffersAt(offset + i as u64));
        }
        if na != nb {
            return Ok(Comparison::DiffersAt(offset + common as u64));
        }
        if na == 0 {
            return Ok(Comparison::Identical);
        }
        offset += na as u64;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use super::*;
    use ofm_jobs::RealFs;
    use ofm_testkit::tree::{FileSpec, TreeSpec};

    #[test]
    fn identical_different_and_shorter() {
        let big: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
        let mut other = big.clone();
        other[400_000] ^= 0xff;
        let t = TreeSpec::new()
            .file("a", FileSpec::content(&big))
            .file("b", FileSpec::content(&big))
            .file("c", FileSpec::content(&other))
            .file("d", FileSpec::content(&big[..1000]))
            .build_temp()
            .unwrap();
        let p = |n: &str| t.path().join(n);
        let no = AtomicBool::new(false);
        let cmp = |x: &str, y: &str| compare_files(&RealFs, &p(x), &p(y), &no).unwrap();
        assert_eq!(cmp("a", "b"), Comparison::Identical);
        assert_eq!(cmp("a", "c"), Comparison::DiffersAt(400_000));
        assert_eq!(cmp("a", "d"), Comparison::DiffersAt(1000));
        assert_eq!(cmp("d", "a"), Comparison::DiffersAt(1000));
        let yes = AtomicBool::new(true);
        assert_eq!(
            compare_files(&RealFs, &p("a"), &p("b"), &yes).unwrap(),
            Comparison::Cancelled
        );
        assert!(compare_files(&RealFs, &p("a"), &p("missing"), &no).is_err());
    }
}
