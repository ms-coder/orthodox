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
/// parent directories. A failure leaves the old file untouched. The file gets the default
/// permissions (umask); use [`atomic_write_private`] for files that may hold secrets. If `path`
/// is a symlink the real target is written and the symlink is preserved (a dangling symlink is
/// an error, not silently replaced).
pub fn atomic_write(path: &Path, contents: &str) -> Result<(), ConfigError> {
    atomic_write_with_mode(path, contents, None)
}

/// [`atomic_write`] for private data (command history may contain secrets typed on the command
/// line): the file is created with mode `0600`, regardless of the umask, and an existing
/// world-readable file is replaced by a private one.
pub fn atomic_write_private(path: &Path, contents: &str) -> Result<(), ConfigError> {
    atomic_write_with_mode(path, contents, Some(0o600))
}

fn atomic_write_with_mode(
    path: &Path,
    contents: &str,
    mode: Option<u32>,
) -> Result<(), ConfigError> {
    // A symlinked config file must keep pointing at its real target (dotfile managers symlink
    // config.toml into a repo): resolve the final component and write/rename over the target so
    // the symlink survives (T8.8). A dangling symlink is an error rather than silently
    // replacing the user's symlink with a regular file.
    let target: std::path::PathBuf = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|e| io_err(path, e))?
        }
        Ok(_) => path.to_path_buf(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(io_err(path, e)),
    };
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(target.file_name().unwrap_or_default());
    tmp_name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(mode);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &target)
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

/// [`save_toml`] with the private (`0600`) file mode of [`atomic_write_private`].
pub fn save_toml_private<T: serde::Serialize>(file: &Path, value: &T) -> Result<(), ConfigError> {
    let text = toml::to_string_pretty(value).map_err(|e| ConfigError::Serialize(e.to_string()))?;
    atomic_write_private(file, &text)
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
    fn private_write_is_0600_and_tightens_an_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s/history.toml");
        atomic_write_private(&p, "one").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&p), 0o600);
        // An old world-readable file is replaced by a private one.
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        atomic_write_private(&p, "two").unwrap();
        assert_eq!(mode(&p), 0o600);
        assert_eq!(fs::read_to_string(&p).unwrap(), "two");
        assert_eq!(fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
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

    #[test]
    fn symlinked_target_is_preserved() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let d = tempfile::tempdir().unwrap();
        // Dotfile-manager layout: config.toml is a symlink into a repo directory.
        let real = d.path().join("repo/config.toml");
        fs::create_dir(real.parent().unwrap()).unwrap();
        atomic_write(&real, "one").unwrap();
        let link = d.path().join("config.toml");
        symlink(&real, &link).unwrap();

        atomic_write(&link, "two").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "two");
        assert_eq!(fs::read_to_string(&link).unwrap(), "two");
        // No regular file replaced the symlink, no temp files left.
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2); // link + repo dir

        // The private variant keeps 0600 semantics on the real target.
        atomic_write_private(&link, "three").unwrap();
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "three");
    }

    #[test]
    fn dangling_symlink_is_an_error_not_replaced() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let link = d.path().join("config.toml");
        symlink(d.path().join("gone.toml"), &link).unwrap();
        assert!(atomic_write(&link, "x").is_err());
        // The symlink is untouched.
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
