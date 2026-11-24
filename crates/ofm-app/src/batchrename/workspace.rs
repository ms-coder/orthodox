//! The private temporary directory that holds the editor text file. Everything here does file
//! I/O: call it from the I/O pool, never from the GTK thread.
//!
//! The directory is created exclusively (`mkdir` fails if the name exists) with mode 0700 and
//! the file with `O_CREAT | O_EXCL | O_NOFOLLOW` and mode 0600 inside it, so no other user can
//! pre-create, read or redirect it. It lives in `$XDG_RUNTIME_DIR` (a private tmpfs) when that is
//! set, else in the system temp directory.

use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Prefix of the directory name; also what [`remove`] insists on before it deletes anything.
pub const DIR_PREFIX: &str = "ofm-batchrename-";

/// Name of the editor text file (its extension lets editors pick a plain-text mode).
pub const FILE_NAME: &str = "names.txt";

/// Largest edited file that is read back (a name list is tiny; this bounds a runaway editor).
const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// The temp directory and the file in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The 0700 directory.
    pub dir: PathBuf,
    /// The 0600 text file the editor opens.
    pub file: PathBuf,
}

/// Where temporary directories are made.
pub fn base_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Create the directory `<base>/ofm-batchrename-<unique>` and the file in it with `text`.
/// `unique` must differ per call in this process; a name that already exists is never reused
/// (the call fails). On failure nothing is left behind.
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn create(base: &Path, unique: &str, text: &str) -> io::Result<Workspace> {
    let dir = base.join(format!("{DIR_PREFIX}{unique}"));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let file = dir.join(FILE_NAME);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&file)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()
    })();
    match result {
        Ok(()) => Ok(Workspace { dir, file }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// Read the edited file as UTF-8 text (refusing symlinks and files over 32 MiB).
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn read_edited(file: &Path) -> io::Result<String> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(file)?;
    let mut bytes = Vec::new();
    f.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the edited file is too large",
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the edited file is not valid UTF-8",
        )
    })
}

/// Delete the workspace (the directory with whatever the editor left in it, e.g. backup files).
/// Refuses any directory that does not carry [`DIR_PREFIX`]. Errors are ignored: a leftover
/// temp directory must never fail a rename.
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn remove(ws: &Workspace) {
    let ours = ws
        .dir
        .file_name()
        .is_some_and(|n| n.as_encoded_bytes().starts_with(DIR_PREFIX.as_bytes()));
    if ours {
        let _ = std::fs::remove_dir_all(&ws.dir);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ofm-ws-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn creates_private_dir_and_file_and_removes_them() {
        let base = tmp();
        let ws = create(&base, "t1", "a\nb\n").unwrap();
        let dir_mode = std::fs::metadata(&ws.dir).unwrap().permissions().mode() & 0o777;
        let file_mode = std::fs::metadata(&ws.file).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        assert_eq!(read_edited(&ws.file).unwrap(), "a\nb\n");
        // The editor may leave backups next to the file.
        std::fs::write(ws.dir.join("names.txt~"), "x").unwrap();
        remove(&ws);
        assert!(!ws.dir.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn an_existing_name_is_never_reused() {
        let base = tmp();
        let first = create(&base, "same", "1").unwrap();
        let err = create(&base, "same", "2").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read_edited(&first.file).unwrap(), "1");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_pre_planted_symlink_is_not_followed() {
        let base = tmp();
        let target = base.join("victim");
        std::fs::write(&target, "keep").unwrap();
        // A symlink where the directory would go: mkdir fails, nothing is written through it.
        std::os::unix::fs::symlink(&target, base.join(format!("{DIR_PREFIX}evil"))).unwrap();
        assert!(create(&base, "evil", "pwn").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        // A symlinked edited file is refused when reading.
        let link = base.join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_edited(&link).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn invalid_utf8_is_reported_not_guessed() {
        let base = tmp();
        let f = base.join("bad.txt");
        std::fs::write(&f, b"ok\n\xff\xfe\n").unwrap();
        let e = read_edited(&f).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn remove_refuses_foreign_directories() {
        let base = tmp();
        let foreign = base.join("not-ours");
        std::fs::create_dir(&foreign).unwrap();
        remove(&Workspace {
            dir: foreign.clone(),
            file: foreign.join("x"),
        });
        assert!(foreign.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
