//! Freedesktop trash (T3.2 owns this file; F14), implemented directly after the Trash
//! specification 1.0 (no `trash` crate: the process-global environment lookups of such crates make
//! it impossible to test against a temporary trash, and the spec is small).
//!
//! Called only through `Fs::trash` (so `FaultyFs` can intercept it); therefore the syscalls here
//! are not routed through `Fs` themselves, except the move of the item itself: it uses the shared
//! no-replace rename of `copy::noreplace` (NFS and many FUSE file systems have no
//! `RENAME_NOREPLACE`), over the `Fs` given to [`trash_path_with_fs`].
//!
//! - Same file system as the home trash (`$XDG_DATA_HOME/Trash`, default `~/.local/share/Trash`):
//!   move there, `Path=` in the `.trashinfo` is absolute.
//! - Other file system: the "top directory" is found by walking up while `st_dev` stays the same;
//!   `$topdir/.Trash/$uid` is used if `$topdir/.Trash` is a real directory (no symlink) with the
//!   sticky bit, else `$topdir/.Trash-$uid` is created (mode 0700). `Path=` is relative to the top
//!   directory.
//! - Nothing usable (cannot create a trash directory, no home, item is already inside a trash):
//!   [`TrashError::Unavailable`], so the engine asks before deleting permanently.
//! - Names are claimed race-free by creating `info/<name>.trashinfo` with `O_EXCL` first, then the
//!   item is moved without replacing (`RENAME_NOREPLACE`, or its fallbacks). A failed move removes
//!   the claimed info file again.
//! - Names too long for `<name>.trashinfo` (`NAME_MAX`) are stored under a bytewise-truncated name
//!   (hash of the full name + extension kept); `Path=` in the `.trashinfo` still records the full
//!   original name, so restoring (GNOME Files & co.) is unaffected.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::copy::noreplace;
use crate::fs::{Fs, RealFs, TrashError};

/// `EXDEV`: the trash directory is on another file system than the item.
const EXDEV: i32 = 18;

/// Where the home trash lives and who the user is. [`TrashEnv::from_process`] reads the process
/// environment; tests build one explicitly (never touching the real trash).
#[derive(Debug, Clone)]
pub struct TrashEnv {
    /// Home trash directory (`$XDG_DATA_HOME/Trash`).
    pub home_trash: PathBuf,
    /// Numeric user id (names `.Trash/$uid` and `.Trash-$uid`).
    pub uid: u32,
}

impl TrashEnv {
    /// `$XDG_DATA_HOME/Trash` (absolute value required, else `$HOME/.local/share/Trash`).
    pub fn from_process() -> Result<TrashEnv, TrashError> {
        let abs = |v: Option<OsString>| v.filter(|v| Path::new(v).is_absolute());
        let data = match abs(std::env::var_os("XDG_DATA_HOME")) {
            Some(d) => PathBuf::from(d),
            None => match abs(std::env::var_os("HOME")) {
                Some(h) => PathBuf::from(h).join(".local/share"),
                None => return Err(TrashError::Unavailable("no home directory".into())),
            },
        };
        Ok(TrashEnv {
            home_trash: data.join("Trash"),
            uid: rustix::process::getuid().as_raw(),
        })
    }
}

/// Move `path` to the trash of the current user.
pub fn trash_path(path: &Path) -> Result<(), TrashError> {
    trash_path_with(&TrashEnv::from_process()?, path)
}

/// A trash directory (`files/` + `info/`) and the directory `Path=` is relative to (`None` =
/// absolute paths, i.e. the home trash).
struct Target {
    dir: PathBuf,
    topdir: Option<PathBuf>,
}

/// Move `path` to the trash described by `env`.
pub fn trash_path_with(env: &TrashEnv, path: &Path) -> Result<(), TrashError> {
    trash_path_with_fs(&RealFs, env, path)
}

/// [`trash_path_with`], moving the item through `mover` (so tests can stand in for a file system
/// without `RENAME_NOREPLACE`).
pub fn trash_path_with_fs(mover: &dyn Fs, env: &TrashEnv, path: &Path) -> Result<(), TrashError> {
    let abs = std::path::absolute(path)?;
    let name = abs
        .file_name()
        .map(OsStr::to_owned)
        .ok_or_else(|| TrashError::Unavailable("cannot trash the root directory".into()))?;
    let parent = abs.parent().unwrap_or(Path::new("/"));
    // Resolve symlinks in the parent only: the item itself may be a symlink and is moved as is.
    let parent = fs::canonicalize(parent)?;
    let full = parent.join(&name);
    let meta = fs::symlink_metadata(&full)?;

    if let Ok(home) = fs::canonicalize(&env.home_trash)
        && full.starts_with(&home)
    {
        return Err(TrashError::Unavailable(
            "item is already in the trash".into(),
        ));
    }

    let mut reasons = Vec::new();
    for target in candidates(env, &parent, &meta) {
        match try_target(mover, &target, &full, &name, meta.is_dir()) {
            Ok(()) => return Ok(()),
            Err(Failure::Unusable(why)) => reasons.push(why),
            Err(Failure::Io(e)) => return Err(TrashError::Io(e)),
        }
    }
    Err(TrashError::Unavailable(if reasons.is_empty() {
        "no trash directory".into()
    } else {
        reasons.join("; ")
    }))
}

enum Failure {
    /// This trash directory cannot be used for the item; try the next candidate / ask the user.
    Unusable(String),
    /// A real I/O error on the item itself.
    Io(io::Error),
}

/// Candidate trash directories in order of preference.
fn candidates(env: &TrashEnv, parent: &Path, meta: &fs::Metadata) -> Vec<Target> {
    let dev = meta.dev();
    // The home trash is used when it is (or can be created) on the item's file system.
    if ensure_dir(&env.home_trash).is_ok()
        && fs::metadata(&env.home_trash).is_ok_and(|m| m.dev() == dev)
    {
        return vec![Target {
            dir: env.home_trash.clone(),
            topdir: None,
        }];
    }
    let top = top_dir(parent, dev);
    let mut out = Vec::new();
    let shared = top.join(".Trash");
    if let Ok(m) = fs::symlink_metadata(&shared) {
        // Spec: must be a directory, not a symlink, with the sticky bit.
        if m.is_dir() && m.mode() & 0o1000 != 0 {
            out.push(Target {
                dir: shared.join(env.uid.to_string()),
                topdir: Some(top.clone()),
            });
        }
    }
    out.push(Target {
        dir: top.join(format!(".Trash-{}", env.uid)),
        topdir: Some(top),
    });
    out
}

/// The mount point of the file system `dev` that contains `start`: the highest ancestor with the
/// same device id.
fn top_dir(start: &Path, dev: u64) -> PathBuf {
    let mut top = start.to_path_buf();
    while let Some(p) = top.parent() {
        match fs::metadata(p) {
            Ok(m) if m.dev() == dev => top = p.to_path_buf(),
            _ => break,
        }
    }
    top
}

/// `mkdir -p` with mode 0700 for newly created directories.
fn ensure_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

fn try_target(
    mover: &dyn Fs,
    target: &Target,
    full: &Path,
    name: &OsStr,
    is_dir: bool,
) -> Result<(), Failure> {
    let unusable = |e: io::Error| Failure::Unusable(format!("{}: {e}", target.dir.display()));
    let files = target.dir.join("files");
    let info = target.dir.join("info");
    ensure_dir(&files).map_err(unusable)?;
    ensure_dir(&info).map_err(unusable)?;

    let recorded = match &target.topdir {
        None => full.to_path_buf(),
        Some(top) => full.strip_prefix(top).unwrap_or(full).to_path_buf(),
    };
    let content = trashinfo(&recorded);

    // Claim a unique name: `name`, `name.2`, `name.3`, ... (shortened when `name` is too long
    // for `<name>.trashinfo`; `Path=` keeps the full original name either way).
    for n in 1u32..=100_000 {
        let candidate = stored_name(name, n);
        let info_path = info.join(append(&candidate, ".trashinfo"));
        let dest = files.join(&candidate);
        let mut f = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&info_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(unusable(e)),
        };
        // An orphan in files/ without info: do not clobber it.
        if fs::symlink_metadata(&dest).is_ok() {
            drop(f);
            let _ = fs::remove_file(&info_path);
            continue;
        }
        if let Err(e) = f.write_all(content.as_bytes()).and_then(|()| f.flush()) {
            drop(f);
            let _ = fs::remove_file(&info_path);
            return Err(Failure::Io(e));
        }
        drop(f);
        return match noreplace::rename_noreplace(mover, full, &dest, is_dir) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&info_path);
                if e.raw_os_error() == Some(EXDEV) {
                    Err(Failure::Unusable(format!("{}: {e}", target.dir.display())))
                } else {
                    Err(Failure::Io(e))
                }
            }
        };
    }
    Err(Failure::Unusable("no free name in the trash".into()))
}

/// The longest name that still fits `NAME_MAX` (255) with a `.trashinfo` suffix appended for the
/// info file; both `files/<stored>` and `info/<stored>.trashinfo` must stay within it.
const STORED_MAX: usize = 255 - ".trashinfo".len();

/// The stored name for claim `n` (>= 1): the original name plus `.n` if that already fits. Long
/// names (which would make `info/<name>.trashinfo` fail with `ENAMETOOLONG`, and thus look like a
/// missing trash) are truncated bytewise: the extension is kept if it is short, and a hash of the
/// full original name keeps distinct long names distinct. Restore works regardless, because
/// `Path=` inside the `.trashinfo` always records the full original name.
fn stored_name(name: &OsStr, n: u32) -> OsString {
    let num = if n <= 1 {
        String::new()
    } else {
        format!(".{n}")
    };
    if name.len() + num.len() <= STORED_MAX {
        return append(name, &num);
    }
    let bytes = name.as_bytes();
    // Extension: the part from the last dot (a leading dot is not an extension) if it is short
    // enough to be worth keeping.
    let ext: &[u8] = match bytes.iter().rposition(|&b| b == b'.') {
        Some(pos) if pos > 0 && bytes.len() - pos <= 16 => &bytes[pos..],
        _ => b"",
    };
    let marker = format!("{:08x}", fnv1a(bytes));
    let stem = STORED_MAX
        .saturating_sub(marker.len() + ext.len() + num.len())
        .min(bytes.len() - ext.len().min(bytes.len()));
    let mut v = bytes[..stem].to_vec();
    v.extend_from_slice(marker.as_bytes());
    v.extend_from_slice(num.as_bytes());
    v.extend_from_slice(ext);
    OsString::from_vec(v)
}

/// FNV-1a over the raw bytes of the name: 32 bit are plenty to keep trash stored names of distinct
/// long names distinct (a collision only costs the next free claim number).
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn append(name: &OsStr, suffix: &str) -> OsString {
    let mut v = name.as_bytes().to_vec();
    v.extend_from_slice(suffix.as_bytes());
    OsString::from_vec(v)
}

/// Content of a `.trashinfo` file.
fn trashinfo(recorded: &Path) -> String {
    let date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={date}\n",
        percent_encode(recorded.as_os_str().as_bytes())
    )
}

/// RFC 3986 percent-encoding of raw bytes (`/` and unreserved characters stay).
fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ofm_testkit::crossdev::two_filesystems;
    use ofm_testkit::temp::TempDir;
    use ofm_testkit::tree::raw_name;
    use std::os::unix::fs::PermissionsExt;

    /// Removes a directory we created outside the temp dirs (mount roots).
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn env_in(dir: &TempDir) -> TrashEnv {
        TrashEnv {
            home_trash: dir.path().join("share/Trash"),
            uid: rustix::process::getuid().as_raw(),
        }
    }

    fn info_of(trash: &Path, name: &str) -> String {
        fs::read_to_string(trash.join("info").join(format!("{name}.trashinfo"))).unwrap()
    }

    #[test]
    fn trashed_file_lands_in_xdg_trash_with_trashinfo() {
        let t = TempDir::new().unwrap();
        let env = env_in(&t);
        let f = t.path().join("my file#1.txt");
        fs::write(&f, b"hello").unwrap();

        trash_path_with(&env, &f).unwrap();

        assert!(!f.exists());
        assert_eq!(
            fs::read(env.home_trash.join("files/my file#1.txt")).unwrap(),
            b"hello"
        );
        let info = info_of(&env.home_trash, "my file#1.txt");
        let lines: Vec<&str> = info.lines().collect();
        assert_eq!(lines[0], "[Trash Info]");
        let canon = fs::canonicalize(t.path()).unwrap();
        let enc = percent_encode(canon.join("my file#1.txt").as_os_str().as_bytes());
        assert_eq!(lines[1], format!("Path={enc}"));
        assert!(lines[1].contains("my%20file%231.txt"));
        let date = lines[2].strip_prefix("DeletionDate=").unwrap();
        assert!(
            chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S").is_ok(),
            "{date}"
        );
        let mode = fs::metadata(&env.home_trash).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn directories_and_symlinks_are_moved_as_they_are() {
        let t = TempDir::new().unwrap();
        let env = env_in(&t);
        let d = t.path().join("d");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub/x"), b"x").unwrap();
        let outside = t.path().join("outside");
        fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, t.path().join("l")).unwrap();

        trash_path_with(&env, &d).unwrap();
        trash_path_with(&env, &t.path().join("l")).unwrap();

        assert!(env.home_trash.join("files/d/sub/x").is_file());
        let moved = env.home_trash.join("files/l");
        assert!(fs::symlink_metadata(&moved).unwrap().is_symlink());
        assert_eq!(
            fs::read(&outside).unwrap(),
            b"keep",
            "link target untouched"
        );
    }

    #[test]
    fn name_collisions_get_numbered_names() {
        let t = TempDir::new().unwrap();
        let env = env_in(&t);
        for i in 0..3 {
            let f = t.path().join("same");
            fs::write(&f, [b'0' + i]).unwrap();
            trash_path_with(&env, &f).unwrap();
        }
        let files = env.home_trash.join("files");
        assert_eq!(fs::read(files.join("same")).unwrap(), b"0");
        assert_eq!(fs::read(files.join("same.2")).unwrap(), b"1");
        assert_eq!(fs::read(files.join("same.3")).unwrap(), b"2");
        assert!(info_of(&env.home_trash, "same.3").contains("Path="));
    }

    #[test]
    fn orphan_in_files_is_not_clobbered() {
        let t = TempDir::new().unwrap();
        let env = env_in(&t);
        fs::create_dir_all(env.home_trash.join("files")).unwrap();
        fs::write(env.home_trash.join("files/f"), b"orphan").unwrap();
        let f = t.path().join("f");
        fs::write(&f, b"new").unwrap();
        trash_path_with(&env, &f).unwrap();
        assert_eq!(fs::read(env.home_trash.join("files/f")).unwrap(), b"orphan");
        assert_eq!(fs::read(env.home_trash.join("files/f.2")).unwrap(), b"new");
        assert!(!env.home_trash.join("info/f.trashinfo").exists());
    }

    #[test]
    fn non_utf8_and_newline_names_roundtrip() {
        let t = TempDir::new().unwrap();
        let env = env_in(&t);
        let f = t.path().join(raw_name(b"bad-\xff\n%name"));
        fs::write(&f, b"1").unwrap();
        trash_path_with(&env, &f).unwrap();
        assert!(!f.exists());
        let info = fs::read_to_string(
            env.home_trash
                .join("info")
                .join(OsString::from_vec(b"bad-\xff\n%name.trashinfo".to_vec())),
        )
        .unwrap();
        assert!(info.contains("bad-%FF%0A%25name"), "{info}");
        assert_eq!(
            info.lines().count(),
            3,
            "newline must not leak into the file"
        );
    }

    #[test]
    fn missing_item_is_an_io_error() {
        let t = TempDir::new().unwrap();
        let env = env_in(&t);
        let err = trash_path_with(&env, &t.path().join("nope")).unwrap_err();
        assert!(
            matches!(&err, TrashError::Io(e) if e.kind() == io::ErrorKind::NotFound),
            "{err:?}"
        );
    }

    #[test]
    fn item_inside_the_trash_is_unavailable() {
        let t = TempDir::new().unwrap();
        let env = env_in(&t);
        let f = t.path().join("f");
        fs::write(&f, b"x").unwrap();
        trash_path_with(&env, &f).unwrap();
        let inside = env.home_trash.join("files/f");
        assert!(matches!(
            trash_path_with(&env, &inside),
            Err(TrashError::Unavailable(_))
        ));
        assert!(inside.exists());
    }

    #[test]
    fn other_mount_uses_topdir_trash_with_relative_path() {
        let Some(pair) = two_filesystems() else {
            eprintln!("SKIP: no second filesystem");
            return;
        };
        let env = env_in(&pair.first);
        let f = pair.second.path().join("sub dir/file.txt");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, b"data").unwrap();
        let top = top_dir(
            &fs::canonicalize(f.parent().unwrap()).unwrap(),
            pair.second_dev,
        );
        let topdir_trash = top.join(format!(".Trash-{}", env.uid));
        let pre_existing = topdir_trash.exists();
        let _cleanup = (!pre_existing).then(|| Cleanup(topdir_trash.clone()));

        match trash_path_with(&env, &f) {
            Ok(()) => {
                assert!(!f.exists());
                assert!(
                    !env.home_trash.join("files/file.txt").exists(),
                    "must not cross the device into the home trash"
                );
                let moved = topdir_trash.join("files/file.txt");
                assert!(moved.exists() || pre_existing);
                let rel = f.strip_prefix(&top).unwrap();
                let enc = percent_encode(rel.as_os_str().as_bytes());
                assert!(
                    !enc.starts_with('/'),
                    "topdir trash records a relative path"
                );
                let info = info_of(&topdir_trash, "file.txt");
                assert!(info.contains(&format!("Path={enc}\n")), "{info}");
            }
            // The mount root may be unwritable (e.g. /media/x): Unavailable is the specified
            // answer, and the item must be intact.
            Err(TrashError::Unavailable(_)) => assert!(f.exists()),
            Err(e) => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn sticky_shared_trash_dir_is_preferred() {
        let Some(pair) = two_filesystems() else {
            eprintln!("SKIP: no second filesystem");
            return;
        };
        let env = env_in(&pair.first);
        let top = top_dir(
            &fs::canonicalize(pair.second.path()).unwrap(),
            pair.second_dev,
        );
        let shared = top.join(".Trash");
        let own = top.join(format!(".Trash-{}", env.uid));
        if fs::symlink_metadata(&shared).is_ok() || fs::symlink_metadata(&own).is_ok() {
            eprintln!("SKIP: {} or {} exists", shared.display(), own.display());
            return;
        }
        if fs::create_dir(&shared).is_err() {
            eprintln!("SKIP: cannot create {}", shared.display());
            return;
        }
        let _c1 = Cleanup(shared.clone());
        let _c2 = Cleanup(own.clone());
        let f = pair.second.path().join("file.txt");
        fs::write(&f, b"d").unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        trash_path_with(&env, &f).unwrap();
        assert!(
            shared
                .join(env.uid.to_string())
                .join("files/file.txt")
                .exists()
        );
        // Without the sticky bit the shared dir is ignored (spec) and .Trash-$uid is used.
        let g = pair.second.path().join("g.txt");
        fs::write(&g, b"d").unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        trash_path_with(&env, &g).unwrap();
        assert!(own.join("files/g.txt").exists());
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(percent_encode(b"/a b/\xc3\xa4~._-"), "/a%20b/%C3%A4~._-");
    }
}
