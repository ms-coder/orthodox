//! Helpers for cross-device tests (EXDEV paths of move/copy).

use std::io;
use std::path::{Path, PathBuf};

use crate::temp::TempDir;

/// Two temp directories that live on different filesystems (different `st_dev`).
///
/// Kept alive as long as the test needs them; both are removed when dropped.
#[derive(Debug)]
pub struct FilesystemPair {
    /// Directory on the first filesystem (usually the system temp dir).
    pub first: TempDir,
    /// Directory on a filesystem different from `first`.
    pub second: TempDir,
    /// `st_dev` of `first`.
    pub first_dev: u64,
    /// `st_dev` of `second` (guaranteed `!= first_dev`).
    pub second_dev: u64,
}

/// Device id (`st_dev`) of `path` (follows symlinks, per `stat` semantics).
///
/// # Example
/// ```
/// use ofm_testkit::crossdev::st_dev;
///
/// let dev = st_dev(std::path::Path::new("/")).unwrap();
/// assert_ne!(dev, 0);
/// ```
pub fn st_dev(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.dev())
}

/// Candidate roots probed for a second filesystem, in order.
fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = vec![std::env::temp_dir(), PathBuf::from("/dev/shm")];
    if let Ok(x) = std::env::var("XDG_RUNTIME_DIR") {
        roots.push(PathBuf::from(x));
    }
    roots.push(PathBuf::from("/var/tmp"));
    if let Ok(home) = std::env::var("HOME") {
        roots.push(PathBuf::from(home));
    }
    roots
}

/// Try to provide two temp dirs on two different filesystems, so a test can
/// exercise the cross-device path (EXDEV) of move/copy.
///
/// Returns `None` when only one filesystem is available — callers should skip
/// the test with a log line in that case (never fail):
///
/// ```
/// use ofm_testkit::crossdev::two_filesystems;
///
/// match two_filesystems() {
///     Some(pair) => {
///         assert_ne!(pair.first_dev, pair.second_dev);
///         // ... run the cross-device move here ...
///     }
///     None => eprintln!("SKIP: no second filesystem available"),
/// }
/// ```
pub fn two_filesystems() -> Option<FilesystemPair> {
    let mut first: Option<(TempDir, u64)> = None;
    for root in candidate_roots() {
        let dir = match TempDir::in_dir_named(&root, "ofm-crossdev") {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(root = %root.display(), error = %e, "crossdev: candidate unusable");
                continue;
            }
        };
        let dev = match st_dev(dir.path()) {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(root = %root.display(), error = %e, "crossdev: stat failed");
                continue;
            }
        };
        if first.is_none() {
            first = Some((dir, dev));
            continue;
        }
        let (_, first_dev) = first.as_ref()?;
        if *first_dev == dev {
            tracing::debug!(root = %root.display(), dev, "crossdev: same filesystem as first");
            continue;
        }
        let (first_dir, first_dev) = first?;
        tracing::debug!(
            first = %first_dir.path().display(),
            second = %dir.path().display(),
            "crossdev: found two filesystems"
        );
        return Some(FilesystemPair {
            first: first_dir,
            first_dev,
            second: dir,
            second_dev: dev,
        });
    }
    tracing::debug!("crossdev: no second filesystem found; cross-device tests skip");
    None
}
