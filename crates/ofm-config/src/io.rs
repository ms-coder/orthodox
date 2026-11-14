//! Atomic file writes and tolerant reads.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{ConfigError, Warning};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn io_err(path: &Path, source: std::io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Write `contents` to `path` atomically: temp file in the same directory, fsync, rename; creates
/// parent directories. A failure leaves the old file untouched.
pub fn atomic_write(path: &Path, contents: &str) -> Result<(), ConfigError> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(path.file_name().unwrap_or_default());
    tmp_name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    match result {
        Ok(()) => {
            // Best effort: persist the rename itself.
            if let Ok(d) = fs::File::open(dir) {
                let _ = d.sync_all();
            }
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(io_err(path, e))
        }
    }
}

/// Read a file to a string; `Ok(None)` if it does not exist.
pub fn read_optional(path: &Path) -> Result<Option<String>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path, e)),
    }
}

/// Load a TOML state file into `T`; a missing file gives `T::default()`, an unreadable or invalid
/// one gives `T::default()` plus a [`Warning`].
pub fn load_toml<T: Default + serde::de::DeserializeOwned>(file: &Path) -> (T, Vec<Warning>) {
    let fail = |message: String| {
        (
            T::default(),
            vec![Warning {
                file: Some(file.to_path_buf()),
                message: format!("{message}; using defaults"),
            }],
        )
    };
    match read_optional(file) {
        Ok(None) => (T::default(), Vec::new()),
        Err(e) => fail(e.to_string()),
        Ok(Some(text)) => match toml::from_str(&text) {
            Ok(v) => (v, Vec::new()),
            Err(e) => fail(e.to_string()),
        },
    }
}

/// Serialize `value` and write it atomically to `file`.
pub fn save_toml<T: serde::Serialize>(file: &Path, value: &T) -> Result<(), ConfigError> {
    let text = toml::to_string_pretty(value).map_err(|e| ConfigError::Serialize(e.to_string()))?;
    atomic_write(file, &text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn write_read_replace_and_no_leftovers() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a/b/f.toml");
        assert_eq!(read_optional(&p).unwrap(), None);
        atomic_write(&p, "one").unwrap();
        atomic_write(&p, "two").unwrap();
        assert_eq!(read_optional(&p).unwrap().as_deref(), Some("two"));
        let names: Vec<_> = fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn failed_write_keeps_old_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        atomic_write(&p, "old").unwrap();
        // Target is a directory: rename fails, nothing is left behind.
        let q = d.path().join("dir");
        fs::create_dir(&q).unwrap();
        assert!(atomic_write(&q, "x").is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "old");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2);
    }

    #[test]
    fn read_optional_reports_other_errors() {
        let d = tempfile::tempdir().unwrap();
        assert!(read_optional(d.path()).is_err());
    }
}
