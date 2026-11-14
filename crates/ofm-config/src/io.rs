//! Atomic file writes and tolerant reads.

use std::path::Path;

use crate::error::ConfigError;

/// Write `contents` to `path` atomically: temp file in the same directory, fsync, rename; creates
/// parent directories. A failure leaves the old file untouched.
pub fn atomic_write(_path: &Path, _contents: &str) -> Result<(), ConfigError> {
    todo!("T1.3")
}

/// Read a file to a string; `Ok(None)` if it does not exist.
pub fn read_optional(_path: &Path) -> Result<Option<String>, ConfigError> {
    todo!("T1.3")
}
