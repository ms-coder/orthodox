//! User menu (F28): a list of named commands with placeholders, loaded from the global
//! `menu.toml` (`ConfigPaths::menu_file`) and, per directory, from a `.ofm-menu.toml`
//! (local entries are listed first, see [`UserMenu::merged`]).
//!
//! Placeholder *expansion* lives in `ofm_core::menu_expand` (quoting, prompts, safety
//! checks); this module owns loading, defaults, entry validation and the **trust store**:
//! a local `.ofm-menu.toml` can run arbitrary commands, so it is only used after the user
//! trusted it ([`TrustStore`]). Trust is recorded per canonical file path plus the sha256
//! of the file content — changed content is untrusted again until re-trusted.
//!
//! Load policy (ADR-0005): a missing or invalid file never fails the app. The global menu
//! falls back to the built-in defaults plus a [`Warning`]; problems inside single entries
//! (unknown placeholder, bad hotkey, invalid mask, …) become warnings with line information
//! while the rest of the menu is kept.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ofm_core::mask::{Mask, MaskOpts};
use ofm_core::menu_expand;
use serde::{Deserialize, Serialize};

use crate::defaults;
use crate::error::{ConfigError, Warning};
use crate::io;
use crate::paths::ConfigPaths;

/// Fresh temp names for the default-menu installation (see [`write_temp`]).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// File name of a per-directory menu.
pub const LOCAL_MENU_FILE: &str = ".ofm-menu.toml";

/// Whole menu file (global `menu.toml` or local `.ofm-menu.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserMenu {
    /// Schema version (1).
    pub version: u32,
    /// Items in display order.
    pub item: Vec<MenuItem>,
}

/// One menu item. The command is a template with the F28 placeholders (`%f %s %d %D %F %n
/// %{input:Prompt}`, `%%`), expanded by `ofm_core::menu_expand` at run time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MenuItem {
    /// Hotkey inside the menu (empty = none); validated to a single `char`.
    pub key: String,
    /// Title shown in the menu.
    pub title: String,
    /// Command template.
    pub command: String,
    /// Only show when the cursor file matches this mask (empty = always; `ofm_core::mask`
    /// syntax).
    pub mask: String,
    /// Run in the embedded terminal (`false` = run in the background, output hidden).
    pub terminal: bool,
    /// Ask for confirmation before running (destructive commands).
    pub confirm: bool,
}

impl UserMenu {
    /// The built-in default menu; also the fallback for a broken user file. An invalid
    /// built-in file is a bug in our release and degrades to an empty menu plus a warning.
    pub fn builtin() -> (UserMenu, Vec<Warning>) {
        match toml::from_str(defaults::MENU) {
            Ok(menu) => (menu, Vec::new()),
            Err(e) => (
                UserMenu::default(),
                vec![Warning {
                    file: None,
                    message: format!("built-in default menu is invalid: {e}"),
                }],
            ),
        }
    }

    /// The global menu: the user's `menu.toml` when present and parseable (entry-level
    /// problems yield warnings and are kept), else the built-in defaults plus a warning.
    /// Never writes anything — see [`install_default_global`].
    pub fn load_global(paths: &ConfigPaths) -> (UserMenu, Vec<Warning>) {
        let file = paths.menu_file();
        match io::read_optional(&file) {
            Ok(None) => Self::builtin(),
            Ok(Some(text)) => match toml::from_str::<UserMenu>(&text) {
                Ok(menu) => {
                    let warnings = validate_menu(&menu, &text, &file);
                    (menu, warnings)
                }
                Err(e) => fallback(file, format!("{e}; using the built-in default menu")),
            },
            Err(e) => fallback(file, format!("{e}; using the built-in default menu")),
        }
    }

    /// Merge for display: local entries first, then global (F28 order). The version is the
    /// global one's.
    pub fn merged(local: Option<UserMenu>, global: UserMenu) -> UserMenu {
        let mut out = global;
        if let Some(local) = local {
            let mut items = local.item;
            items.append(&mut out.item);
            out.item = items;
        }
        out
    }
}

fn fallback(file: PathBuf, message: String) -> (UserMenu, Vec<Warning>) {
    let (menu, _) = UserMenu::builtin();
    (
        menu,
        vec![Warning {
            file: Some(file),
            message,
        }],
    )
}

/// How the freshly written temp file is moved to its final name without ever replacing an
/// existing one. Production starts at [`Publish::HardLink`] and walks down the chain on
/// filesystems that lack support (finding 5); tests force a specific layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// `link(2)`: atomic no-clobber wherever hard links exist.
    HardLink,
    /// `renameat2(2)` with `RENAME_NOREPLACE`: same guarantee, different primitive.
    RenameNoReplace,
    /// `open(2)` with `O_EXCL` on the final name: works everywhere (FAT, FUSE), but the
    /// file already exists while it is written, so a reader in that window sees an
    /// incomplete menu — a tiny, documented compromise. An existing file is still never
    /// touched.
    CreateNew,
}

/// Create the global `menu.toml` with the built-in defaults, but only when it does not
/// exist yet (first use). Returns whether the file was created; an existing file — a user's
/// own menu or leftovers of a crashed write — is never touched. Publication prefers
/// `link(2)` (fails atomically when the target already exists, so not even a race between
/// two starting instances can overwrite anything) and degrades on filesystems without hard
/// links (FAT, some FUSE mounts) through a no-clobber rename to `O_EXCL` on the final name;
/// see [`Publish`].
pub fn install_default_global(paths: &ConfigPaths) -> Result<bool, ConfigError> {
    install_with(paths, Publish::HardLink)
}

/// [`install_default_global`] starting publication at `first` — the entry point that lets
/// tests force the hard-link-free layers.
fn install_with(paths: &ConfigPaths, first: Publish) -> Result<bool, ConfigError> {
    let file = paths.menu_file();
    let dir = match file.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir).map_err(|e| ConfigError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;
    let tmp = write_temp(dir, &file)?;
    let result = publish(&tmp, &file, first);
    // No-op once the temp was linked/renamed away; cleans up after a failed publication.
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Write the default menu to a fresh `.<name>.default.<pid>.<n>` temp file in `dir`
/// (`create_new` + fsync). A stale leftover that already owns the name (recycled pid) is
/// removed and retried once; the name embeds our pid, so removing it cannot hit another
/// running instance.
fn write_temp(dir: &Path, file: &Path) -> Result<PathBuf, ConfigError> {
    for _ in 0..2 {
        let mut name = std::ffi::OsString::from(".");
        name.push(file.file_name().unwrap_or_default());
        name.push(format!(
            ".default.{}.{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = dir.join(name);
        let wrote = (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(defaults::MENU.as_bytes())?;
            f.sync_all()
        })();
        match wrote {
            Ok(()) => return Ok(tmp),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&tmp);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(ConfigError::Io {
                    path: file.to_path_buf(),
                    source: e,
                });
            }
        }
    }
    Err(ConfigError::Io {
        path: dir.to_path_buf(),
        source: std::io::Error::other("the default-menu temp name is repeatedly taken"),
    })
}

/// Publish `tmp` as `file`, trying the strategies from `first` downwards. Only "not
/// supported here" errors (FAT refusing hard links, a filesystem without `RENAME_NOREPLACE`)
/// fall through to the next layer; anything real (permissions, full disk) is returned.
fn publish(tmp: &Path, file: &Path, first: Publish) -> Result<bool, ConfigError> {
    let chain: &[Publish] = match first {
        Publish::HardLink => &[
            Publish::HardLink,
            Publish::RenameNoReplace,
            Publish::CreateNew,
        ],
        Publish::RenameNoReplace => &[Publish::RenameNoReplace, Publish::CreateNew],
        Publish::CreateNew => &[Publish::CreateNew],
    };
    for how in chain {
        match publish_one(tmp, file, *how) {
            Ok(created) => return Ok(created),
            Err(e) if unsupported_for_publication(&e) => continue,
            Err(e) => return Err(e),
        }
    }
    // Invariant: `chain` is never empty, so the loop always returned.
    unreachable!("publication chain is never empty")
}

/// One publication attempt; `Ok(false)` = the file already existed and was left alone.
fn publish_one(tmp: &Path, file: &Path, how: Publish) -> Result<bool, ConfigError> {
    let io = |e: std::io::Error| ConfigError::Io {
        path: file.to_path_buf(),
        source: e,
    };
    match how {
        Publish::HardLink => match std::fs::hard_link(tmp, file) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(io(e)),
        },
        Publish::RenameNoReplace => match rename_noreplace(tmp, file) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(io(e)),
        },
        Publish::CreateNew => {
            let mut f = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(file)
            {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
                Err(e) => return Err(io(e)),
            };
            f.write_all(defaults::MENU.as_bytes())
                .and_then(|()| f.sync_all())
                .map_err(io)?;
            Ok(true)
        }
    }
}

/// Errors that mean "this filesystem/kernel does not have that primitive" (finding 5):
/// publication degrades to the next layer. Everything else is a real failure.
fn unsupported_for_publication(err: &ConfigError) -> bool {
    match err {
        ConfigError::Io { source, .. } => matches!(
            source.raw_os_error(),
            Some(libc::EPERM)
                | Some(libc::EMLINK)
                | Some(libc::ENOSYS)
                | Some(libc::EOPNOTSUPP)
                | Some(libc::EXDEV)
                | Some(libc::EINVAL)
        ),
        _ => false,
    }
}

/// `renameat2(2)` with `RENAME_NOREPLACE`, via the raw syscall (glibc's wrapper post-dates
/// some setups we support): renames without ever replacing an existing destination.
#[cfg(unix)]
fn rename_noreplace(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    // Kernel flag (include/uapi/linux/fs.h); the libc crate does not export it.
    const RENAME_NOREPLACE: libc::c_ulong = 1;
    let c_path = |p: &Path| -> std::io::Result<std::ffi::CString> {
        std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains a NUL byte")
        })
    };
    let src = c_path(src)?;
    let dst = c_path(dst)?;
    let res = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            src.as_ptr(),
            libc::AT_FDCWD,
            dst.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if res == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Platforms without `renameat2` (non-unix only) report `ENOSYS` so publication falls
/// through to `O_EXCL`.
#[cfg(not(unix))]
fn rename_noreplace(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
}

/// `dir`/`.ofm-menu.toml`.
pub fn local_menu_path(dir: &Path) -> PathBuf {
    dir.join(LOCAL_MENU_FILE)
}

/// Largest `.ofm-menu.toml` we are willing to read (finding 6): the file sits in every
/// browsed directory and is read on panel updates, so it must stay small.
const MAX_LOCAL_MENU_BYTES: usize = 256 * 1024;

/// Read a local menu file with a bounded, regular-files-only strategy (finding 6).
/// `Ok(None)` = the file does not exist. On unix the file is opened with `O_NONBLOCK` (a
/// FIFO without a writer must not wedge the panel thread), `O_NOFOLLOW` (a symlinked menu
/// is refused: trust is bound to one canonical file, and following the link would hash
/// whatever it currently points at) and `O_NOCTTY`; `fstat` on the descriptor then rejects
/// special files and oversize content before a byte is read, and the read itself stays
/// capped for the file-grew-in-between case. Every refusal is [`ConfigError::ReadRefused`]
/// with a message that says what the file is.
fn read_local_menu_bounded(file: &Path) -> Result<Option<Vec<u8>>, ConfigError> {
    let refused = |message: String| ConfigError::ReadRefused {
        path: file.to_path_buf(),
        message,
    };
    let oversize = |len: u64| {
        refused(format!(
            "is {len} bytes, larger than the {} KiB limit for local menus; not loaded",
            MAX_LOCAL_MENU_BYTES / 1024
        ))
    };
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_NOCTTY);
        let f = match opts.open(file) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // O_NOFOLLOW surfaces a symlink as ELOOP; say what that means.
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Err(refused(
                    "is a symlink, not a regular file; not loaded".into(),
                ));
            }
            Err(e) => {
                return Err(ConfigError::Io {
                    path: file.to_path_buf(),
                    source: e,
                });
            }
        };
        let meta = f.metadata().map_err(|e| ConfigError::Io {
            path: file.to_path_buf(),
            source: e,
        })?;
        if !meta.file_type().is_file() {
            return Err(refused(format!(
                "is a {}, not a regular file; not loaded",
                file_kind(&meta)
            )));
        }
        if meta.len() > MAX_LOCAL_MENU_BYTES as u64 {
            return Err(oversize(meta.len()));
        }
        // The file may grow between fstat and read, so the read is capped as well.
        let mut bytes = Vec::new();
        f.take(MAX_LOCAL_MENU_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| ConfigError::Io {
                path: file.to_path_buf(),
                source: e,
            })?;
        if bytes.len() > MAX_LOCAL_MENU_BYTES {
            return Err(oversize(bytes.len() as u64));
        }
        Ok(Some(bytes))
    }
    #[cfg(not(unix))]
    {
        // Minimal fallback for non-unix builds (OFM targets Linux): same limits, checked
        // around a plain read.
        let meta = match std::fs::metadata(file) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(ConfigError::Io {
                    path: file.to_path_buf(),
                    source: e,
                });
            }
        };
        if !meta.file_type().is_file() {
            return Err(refused("is not a regular file; not loaded".into()));
        }
        if meta.len() > MAX_LOCAL_MENU_BYTES as u64 {
            return Err(oversize(meta.len()));
        }
        let bytes = std::fs::read(file).map_err(|e| ConfigError::Io {
            path: file.to_path_buf(),
            source: e,
        })?;
        if bytes.len() > MAX_LOCAL_MENU_BYTES {
            return Err(oversize(bytes.len() as u64));
        }
        Ok(Some(bytes))
    }
}

/// Human name of a non-regular file's type, for [`ConfigError::ReadRefused`] messages.
#[cfg(unix)]
fn file_kind(meta: &std::fs::Metadata) -> &'static str {
    use std::os::unix::fs::FileTypeExt;
    let t = meta.file_type();
    if t.is_fifo() {
        "FIFO"
    } else if t.is_socket() {
        "socket"
    } else if t.is_dir() {
        "directory"
    } else if t.is_char_device() {
        "character device"
    } else if t.is_block_device() {
        "block device"
    } else {
        "special file"
    }
}

/// Parse local-menu text with entry validation (line information relative to `text`).
/// Used by [`TrustStore::local_menu_status`]; exposed for previews and tests.
pub fn parse_local(text: &str, file: &Path) -> (UserMenu, Vec<Warning>) {
    match toml::from_str::<UserMenu>(text) {
        Ok(menu) => {
            let warnings = validate_menu(&menu, text, file);
            (menu, warnings)
        }
        Err(e) => (
            UserMenu::default(),
            vec![Warning {
                file: Some(file.to_path_buf()),
                message: format!("{e}; menu ignored"),
            }],
        ),
    }
}

/// Trust state of a directory's `.ofm-menu.toml` ([`TrustStore::local_menu_status`]).
#[derive(Debug, Clone, PartialEq)]
pub enum LocalMenuStatus {
    /// No local menu file in this directory.
    Absent,
    /// The file exists but its current content is not trusted (never trusted, or changed
    /// since it was): `hash` is the sha256 of the current bytes — pass it to
    /// [`TrustStore::trust`] once the user approves. `menu` is the parsed content, for a
    /// "what would this run?" review dialog.
    Untrusted {
        /// sha256 of the current file content, lowercase hex.
        hash: String,
        /// The parsed menu (empty, with warnings, when the file is invalid TOML).
        menu: UserMenu,
    },
    /// Trusted for the current content: the menu and its validation warnings.
    Trusted {
        /// The parsed menu.
        menu: UserMenu,
        /// Entry-level problems (kept as warnings, the menu is still usable).
        warnings: Vec<Warning>,
    },
}

/// Records which local `.ofm-menu.toml` files the user has trusted (F28). Persisted as a
/// TOML file under the XDG state dir with mode `0600`: it is an allow-list for running
/// arbitrary commands, so it must not be writable by anyone else.
///
/// A record is `(canonical path of the menu file, sha256 of its content)`; a file whose
/// content changed is [`LocalMenuStatus::Untrusted`] again with the new hash until
/// re-trusted. Records are never pruned in v1.0 (they are two lines per trusted directory).
pub struct TrustStore {
    file: PathBuf,
    trusted: BTreeMap<PathBuf, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct TrustFile {
    /// Schema version (1).
    version: u32,
    #[serde(default)]
    trusted: Vec<TrustRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct TrustRecord {
    /// Percent-encoded canonical path of the menu file (exact for non-UTF-8 paths).
    path: String,
    /// sha256 of the trusted content, lowercase hex.
    sha256: String,
}

impl TrustStore {
    /// Load the store from `<state>/menu-trust.toml`; a missing file is an empty store, an
    /// invalid one an empty store plus warnings (single bad records are dropped, the rest
    /// is kept).
    pub fn load(paths: &ConfigPaths) -> (TrustStore, Vec<Warning>) {
        let file = paths.state_dir.join("menu-trust.toml");
        let (parsed, mut warnings) = io::load_toml::<TrustFile>(&file);
        // A missing file deserializes to version 0 (defaults) — nothing to warn about.
        if parsed.version > 1 {
            warnings.push(Warning {
                file: Some(file.clone()),
                message: format!("unknown menu-trust schema version {}", parsed.version),
            });
        }
        let mut trusted = BTreeMap::new();
        for record in parsed.trusted {
            let warning = |message: String| Warning {
                file: Some(file.clone()),
                message,
            };
            let Some(path) = decode_path(&record.path) else {
                warnings.push(warning(format!(
                    "undecodable trusted path {:?}; dropped",
                    record.path
                )));
                continue;
            };
            if unhex(&record.sha256).is_none() {
                warnings.push(warning(format!(
                    "trusted entry for {} has no valid sha256; dropped",
                    record.path
                )));
                continue;
            }
            trusted.insert(path, record.sha256);
        }
        (TrustStore { file, trusted }, warnings)
    }

    /// Trust state of `dir`'s `.ofm-menu.toml`: [`LocalMenuStatus::Absent`] without a file,
    /// an I/O error only for real failures (unreadable file, …). The file is read with
    /// [`read_local_menu_bounded`].
    pub fn local_menu_status(&self, dir: &Path) -> Result<LocalMenuStatus, ConfigError> {
        let file = local_menu_path(dir);
        let bytes = match read_local_menu_bounded(&file) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(LocalMenuStatus::Absent),
            Err(e) => return Err(e),
        };
        let hash = hex(&sha256::digest(&bytes));
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let (menu, warnings) = parse_local(&text, &file);
        // Canonical key when possible; the plain path only if canonicalization raced a
        // delete. Trust is bound to the content hash, so a weaker key cannot make an
        // untrusted file trusted — at worst it records under two names.
        let key = file.canonicalize().unwrap_or(file);
        if self.trusted.get(&key).is_some_and(|h| *h == hash) {
            Ok(LocalMenuStatus::Trusted { menu, warnings })
        } else {
            Ok(LocalMenuStatus::Untrusted { hash, menu })
        }
    }

    /// Record that the user trusts the local menu file in `dir` with the content that
    /// hashed to `hash` (taken from [`LocalMenuStatus::Untrusted`]). Persisted immediately.
    pub fn trust(&mut self, dir: &Path, hash: &str) -> Result<(), ConfigError> {
        if unhex(hash).is_none() {
            return Err(ConfigError::Parse {
                path: self.file.clone(),
                message: format!("not a sha256 digest: {hash:?}"),
            });
        }
        let key = local_menu_path(dir)
            .canonicalize()
            .unwrap_or_else(|_| local_menu_path(dir));
        self.trusted.insert(key, hash.to_owned());
        self.save()
    }

    fn save(&self) -> Result<(), ConfigError> {
        let file = TrustFile {
            version: 1,
            trusted: self
                .trusted
                .iter()
                .map(|(path, hash)| TrustRecord {
                    path: encode_path(path),
                    sha256: hash.clone(),
                })
                .collect(),
        };
        io::save_toml_private(&self.file, &file)
    }
}

/// Entry-level validation; never fatal — every problem becomes a warning pointing at the
/// entry (by `[[item]]` line when the file uses the usual array-of-tables shape, else by
/// index) and the menu is kept.
fn validate_menu(menu: &UserMenu, text: &str, file: &Path) -> Vec<Warning> {
    let mut warnings = Vec::new();
    if menu.version != 1 {
        warnings.push(Warning {
            file: Some(file.to_path_buf()),
            message: format!("unknown menu schema version {} (expected 1)", menu.version),
        });
    }
    let headers = item_header_lines(text);
    let mut used: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
    for (i, item) in menu.item.iter().enumerate() {
        let place = if headers.len() == menu.item.len() {
            format!("line {}: ", headers[i])
        } else {
            format!("item {}: ", i + 1)
        };
        let title = if item.title.is_empty() {
            "<no title>"
        } else {
            &item.title
        };
        let mut warn = |message: String| {
            warnings.push(Warning {
                file: Some(file.to_path_buf()),
                message: format!("{place}'{title}': {message}"),
            })
        };
        if item.title.is_empty() {
            warn("empty title".to_string());
        }
        if item.command.is_empty() {
            warn("empty command".to_string());
        }
        if let Err(e) = menu_expand::validate_template(&item.command) {
            warn(format!("invalid command: {e}"));
        }
        if !item.mask.is_empty()
            && let Err(e) = Mask::parse(&item.mask, MaskOpts::default())
        {
            warn(format!("invalid mask: {e}"));
        }
        if item.key.is_empty() {
            continue;
        }
        let mut chars = item.key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if let Some(first) = used.get(&c) {
                warn(format!("hotkey '{c}' already used by item {}", first + 1));
            } else {
                used.insert(c, i);
            }
        } else {
            warn(format!(
                "hotkey must be a single character, got {:?}",
                item.key
            ));
        }
    }
    warnings
}

/// Line numbers (1-based) of the `[[item]]` headers in `text`, in order — only to point
/// warnings at an entry. When their count does not match the parsed array (inline tables, a
/// `[[item]]` inside a multi-line string) warnings fall back to the item index.
fn item_header_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("[[item]]"))
        .map(|(i, _)| i + 1)
        .collect()
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Parse 64 lowercase hex digits.
fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in s.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

/// Percent-encode a path byte-exactly (readable for the common ASCII case, exact for
/// non-UTF-8 names).
fn encode_path(path: &Path) -> String {
    #[cfg(unix)]
    let bytes: Vec<u8> = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()).to_vec();
    #[cfg(not(unix))]
    let bytes: Vec<u8> = path.to_string_lossy().into_owned().into_bytes();
    let mut out = String::with_capacity(bytes.len());
    for b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Inverse of [`encode_path`].
fn decode_path(s: &str) -> Option<PathBuf> {
    let raw = s.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            let hi = (raw.get(i + 1).copied()? as char).to_digit(16)?;
            let lo = (raw.get(i + 2).copied()? as char).to_digit(16)?;
            bytes.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    os_from_bytes(bytes).map(PathBuf::from)
}

fn os_from_bytes(bytes: Vec<u8>) -> Option<std::ffi::OsString> {
    #[cfg(unix)]
    let os = std::os::unix::ffi::OsStringExt::from_vec(bytes);
    #[cfg(not(unix))]
    let os = std::ffi::OsString::from(String::from_utf8(bytes).ok()?);
    Some(os)
}

/// Minimal SHA-256 (FIPS 180-4) for the trust store: hashing file content to detect
/// changes. Self-contained so `ofm-config` needs no crypto dependency; verified against
/// the standard test vectors in the tests below.
mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    /// SHA-256 of `data`.
    pub fn digest(data: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        // Message with padding: 0x80, zeros to 56 mod 64, then the bit length.
        let mut message = data.to_vec();
        message.push(0x80);
        while message.len() % 64 != 56 {
            message.push(0);
        }
        message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        for block in message.as_chunks::<64>().0 {
            compress(&mut h, block);
        }
        let mut out = [0u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(h: &mut [u32; 8], block: &[u8]) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (hi, round) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *hi = hi.wrapping_add(round);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn paths_for(config: &Path, state: &Path) -> ConfigPaths {
        ConfigPaths::with_dirs(config.to_path_buf(), state.to_path_buf())
    }

    fn write(path: &Path, text: &str) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"The quick brown fox jumps over the lazy dog",
                "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592",
            ),
            // Two blocks (padding spans the boundary).
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(hex(&sha256::digest(input)), *want, "{input:?}");
        }
        // The million-'a' vector exercises many blocks.
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&sha256::digest(&million)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn hex_helpers_round_trip_and_reject() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
        let bytes = [0x12u8; 32];
        assert_eq!(unhex(&hex(&bytes)), Some(bytes));
        for bad in ["", "zz", "ABCD", &"a".repeat(63), &"a".repeat(65)] {
            assert_eq!(unhex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn path_encoding_round_trips_exotic_paths() {
        for path in [
            "/home/user/proj/.ofm-menu.toml",
            "/tmp/sp ace/.ofm-menu.toml",
            "/",
        ] {
            assert_eq!(
                decode_path(&encode_path(Path::new(path))).unwrap(),
                PathBuf::from(path),
                "{path}"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let raw = vec![b'/', 0xff, b'\\', b'\'', 0x00, b'e'];
            let name: std::ffi::OsString = OsStringExt::from_vec(raw.clone());
            let path = PathBuf::from(name);
            let encoded = encode_path(&path);
            assert_eq!(decode_path(&encoded).unwrap(), path, "{encoded}");
            assert!(encoded.is_ascii());
        }
    }

    #[test]
    fn builtin_menu_has_the_shipped_defaults() {
        let (menu, warnings) = UserMenu::builtin();
        assert!(warnings.is_empty());
        let titles: Vec<&str> = menu.item.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "tar.gz selection",
                "rsync selection to other panel",
                "open terminal here"
            ]
        );
        assert_eq!(menu.version, 1);
    }

    #[test]
    fn load_global_missing_file_gives_the_defaults_without_warnings() {
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (menu, warnings) = UserMenu::load_global(&paths_for(cfg.path(), state.path()));
        assert!(warnings.is_empty());
        assert_eq!(menu.item.len(), 3);
        assert!(!cfg.path().join("menu.toml").exists(), "must not write");
    }

    #[test]
    fn load_global_invalid_file_falls_back_to_the_defaults() {
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        write(&cfg.path().join("menu.toml"), "version = ");
        let (menu, warnings) = UserMenu::load_global(&paths_for(cfg.path(), state.path()));
        assert_eq!(menu.item.len(), 3);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0]
                .message
                .contains("using the built-in default menu")
        );
    }

    #[test]
    fn load_global_keeps_a_valid_file_and_reports_entry_problems_with_lines() {
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let text = r#"
version = 1

[[item]]
key = "T"
title = "ok"
command = "du -sh %s"

[[item]]
key = "XY"
title = "bad"
command = "zip %z"

[[item]]
key = "T"
title = ""
command = ""
"#;
        write(&cfg.path().join("menu.toml"), text);
        let (menu, warnings) = UserMenu::load_global(&paths_for(cfg.path(), state.path()));
        assert_eq!(menu.item.len(), 3, "all items kept");
        let messages: Vec<&str> = warnings.iter().map(|w| w.message.as_str()).collect();
        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("line 9:") && m.contains("unknown placeholder")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("line 9:") && m.contains("single character")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("line 14:") && m.contains("already used")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("empty title")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("empty command")),
            "{messages:?}"
        );
        for w in &warnings {
            assert_eq!(
                w.file.as_deref(),
                Some(cfg.path().join("menu.toml").as_path())
            );
        }
    }

    #[test]
    fn validation_covers_masks_and_versions() {
        let text = r#"
version = 2

[[item]]
title = "m"
command = "x %f"
mask = "[broken"
"#;
        let (menu, warnings) = parse_local(text, Path::new("/x/.ofm-menu.toml"));
        assert_eq!(menu.item.len(), 1);
        let messages: Vec<&str> = warnings.iter().map(|w| w.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("schema version 2")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("invalid mask")),
            "{messages:?}"
        );
    }

    #[test]
    fn inline_items_get_index_based_warnings() {
        let text = r#"version = 1
item = [{ title = "t", command = "oops %q" }]
"#;
        let (_, warnings) = parse_local(text, Path::new("/x/.ofm-menu.toml"));
        assert!(
            warnings[0].message.starts_with("item 1:"),
            "{:?}",
            warnings[0]
        );
    }

    #[test]
    fn merged_puts_local_entries_first() {
        let mk = |t: &str| UserMenu {
            version: 1,
            item: vec![MenuItem {
                title: t.to_string(),
                ..MenuItem::default()
            }],
        };
        let merged = UserMenu::merged(Some(mk("local")), mk("global"));
        let titles: Vec<&str> = merged.item.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["local", "global"]);
        assert!(UserMenu::merged(None, mk("global")).item.len() == 1);
    }

    #[test]
    fn install_default_global_creates_once_and_never_overwrites() {
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let paths = paths_for(cfg.path(), state.path());
        let file = cfg.path().join("menu.toml");
        assert!(install_default_global(&paths).unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), defaults::MENU);
        // A user menu (or a leftover from a crash) is never touched.
        write(&file, "version = 1\n");
        assert!(!install_default_global(&paths).unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "version = 1\n");
    }

    #[test]
    fn install_default_global_works_without_hard_link_support() {
        // FAT / some FUSE mounts refuse hard links (finding 5): forced through the
        // no-clobber rename and the O_EXCL layers the default menu still installs once,
        // never overwrites, and leaves no temp files behind.
        for forced in [Publish::RenameNoReplace, Publish::CreateNew] {
            let cfg = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            let paths = paths_for(cfg.path(), state.path());
            let file = cfg.path().join("menu.toml");
            assert!(install_with(&paths, forced).unwrap(), "{forced:?}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), defaults::MENU);
            assert_eq!(std::fs::read_dir(cfg.path()).unwrap().count(), 1);
            write(&file, "version = 1\n");
            assert!(!install_with(&paths, forced).unwrap(), "{forced:?}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "version = 1\n");
            assert_eq!(std::fs::read_dir(cfg.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn trust_store_lifecycle() {
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let paths = paths_for(cfg.path(), state.path());
        let (mut store, warnings) = TrustStore::load(&paths);
        assert!(warnings.is_empty());

        let dir = tempfile::tempdir().unwrap();
        let menu_file = local_menu_path(dir.path());
        assert_eq!(
            store.local_menu_status(dir.path()).unwrap(),
            LocalMenuStatus::Absent
        );

        // Present but never trusted.
        let text = "version = 1\n\n[[item]]\ntitle = \"local\"\ncommand = \"echo %f\"\n";
        write(&menu_file, text);
        let LocalMenuStatus::Untrusted { hash, menu } =
            store.local_menu_status(dir.path()).unwrap()
        else {
            panic!("must be untrusted");
        };
        assert_eq!(hash, hex(&sha256::digest(text.as_bytes())));
        assert_eq!(menu.item.len(), 1);

        // Trust it: parsed menu, then persistent across a reload.
        store.trust(dir.path(), &hash).unwrap();
        match store.local_menu_status(dir.path()).unwrap() {
            LocalMenuStatus::Trusted { menu, warnings } => {
                assert_eq!(menu.item.len(), 1);
                assert!(warnings.is_empty());
            }
            other => panic!("must be trusted, got {other:?}"),
        }
        let trust_file = state.path().join("menu-trust.toml");
        assert!(trust_file.exists());
        let (reloaded, _) = TrustStore::load(&paths);
        assert!(matches!(
            reloaded.local_menu_status(dir.path()).unwrap(),
            LocalMenuStatus::Trusted { .. }
        ));

        // Changed content is untrusted again, with the *new* hash.
        write(&menu_file, &text.replace("echo", "run"));
        let status = store.local_menu_status(dir.path()).unwrap();
        let LocalMenuStatus::Untrusted { hash: hash2, .. } = status else {
            panic!("changed content must be untrusted");
        };
        assert_ne!(hash, hash2);

        // Trusting with a bogus hash is rejected and changes nothing.
        assert!(store.trust(dir.path(), "not-a-hash").is_err());
        assert!(matches!(
            store.local_menu_status(dir.path()).unwrap(),
            LocalMenuStatus::Untrusted { .. }
        ));
    }

    #[test]
    fn trust_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let paths = paths_for(cfg.path(), state.path());
        let (mut store, _) = TrustStore::load(&paths);
        let dir = tempfile::tempdir().unwrap();
        write(&local_menu_path(dir.path()), "version = 1\n");
        store.trust(dir.path(), &"a".repeat(64)).unwrap();
        let trust_file = state.path().join("menu-trust.toml");
        let mode = std::fs::metadata(trust_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn trust_follows_the_canonical_file_via_symlinked_directories() {
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (mut store, _) = TrustStore::load(&paths_for(cfg.path(), state.path()));
        let real = tempfile::tempdir().unwrap();
        write(&local_menu_path(real.path()), "version = 1\n");
        let link_parent = tempfile::tempdir().unwrap();
        let link = link_parent.path().join("link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let LocalMenuStatus::Untrusted { hash, .. } = store.local_menu_status(&link).unwrap()
        else {
            panic!()
        };
        store.trust(&link, &hash).unwrap();
        // The same directory reached through its real path is the same trusted file.
        assert!(matches!(
            store.local_menu_status(real.path()).unwrap(),
            LocalMenuStatus::Trusted { .. }
        ));
    }

    #[test]
    #[cfg(unix)]
    fn trust_works_for_non_utf8_directories() {
        use std::os::unix::ffi::OsStringExt;
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (mut store, _) = TrustStore::load(&paths_for(cfg.path(), state.path()));
        let base = tempfile::tempdir().unwrap();
        let name: std::ffi::OsString = OsStringExt::from_vec(vec![0xff, b'x']);
        let dir = base.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        write(&local_menu_path(&dir), "version = 1\n");
        let LocalMenuStatus::Untrusted { hash, .. } = store.local_menu_status(&dir).unwrap() else {
            panic!()
        };
        store.trust(&dir, &hash).unwrap();
        let (reloaded, warnings) = TrustStore::load(&paths_for(cfg.path(), state.path()));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(matches!(
            reloaded.local_menu_status(&dir).unwrap(),
            LocalMenuStatus::Trusted { .. }
        ));
    }

    #[test]
    fn local_menu_status_reports_read_errors() {
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (store, _) = TrustStore::load(&paths_for(cfg.path(), state.path()));
        let dir = tempfile::tempdir().unwrap();
        // A directory where the menu file should be: reading it is a real error, not Absent.
        std::fs::create_dir(local_menu_path(dir.path())).unwrap();
        assert!(store.local_menu_status(dir.path()).is_err());
    }

    #[test]
    fn untrusted_local_menu_with_invalid_toml_carries_a_warning() {
        let cfg = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (store, _) = TrustStore::load(&paths_for(cfg.path(), state.path()));
        let dir = tempfile::tempdir().unwrap();
        write(&local_menu_path(dir.path()), "nonsense = ");
        let LocalMenuStatus::Untrusted { menu, .. } = store.local_menu_status(dir.path()).unwrap()
        else {
            panic!()
        };
        assert!(menu.item.is_empty());
        let (_, warnings) = parse_local("nonsense = ", &local_menu_path(dir.path()));
        assert!(warnings[0].message.contains("menu ignored"));
    }
}
