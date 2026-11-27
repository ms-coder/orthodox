//! Loading the user menu (F28) — everything here does blocking file I/O and therefore runs on
//! the I/O pool (`mod.rs` calls [`load_blocking`] through `wiring::run_on_pool`), never on the
//! GTK thread. The pure part ([`build_view`]) turns the loaded data into what the dialog shows.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ofm_config::menu::{
    LocalMenuStatus, MenuItem, TrustStore, UserMenu, install_default_global, local_menu_path,
};
use ofm_config::{ConfigPaths, Warning};
use ofm_core::mask::{Mask, MaskOpts};

/// Largest local menu file accepted at all (bytes). The trust prompt shows the whole file
/// (that is what the sha256 covers), so anything larger is refused outright instead of
/// offering trust for content the user cannot see. The read is also bounded: a huge or
/// endless file must not wedge a pool thread.
pub const MAX_MENU_BYTES: usize = 256 * 1024;

/// Template written by "Edit local menu" (valid and empty: the example is commented out).
pub const LOCAL_TEMPLATE: &str = "\
# OFM user menu for this folder (F2). Entries here are listed before the global ones.
# OFM asks you to trust this file the first time (and again whenever it changes),
# because a menu entry can run any command.
# Placeholders: %f file under cursor, %s selected names (or the cursor file), %d this
# folder, %D other panel's folder, %F file in the other panel, %n name without extension,
# %{input:Prompt} asks you, %% a literal percent. Names are shell-quoted for you.
version = 1

# [[item]]
# key = \"B\"                    # hotkey inside the menu
# title = \"Build\"
# command = \"make\"
# mask = \"\"                    # optional: only show when the cursor file matches
# terminal = true              # run in the embedded terminal (false = in the background)
# confirm = false              # show the final command and ask before running
";

/// State of the active folder's `.ofm-menu.toml`.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalSource {
    /// No local menu in this folder.
    None,
    /// Trusted for its current content.
    Trusted {
        /// The parsed local menu.
        menu: UserMenu,
        /// Entry-level problems.
        warnings: Vec<Warning>,
    },
    /// Not trusted (new or changed): the user must decide first.
    Untrusted {
        /// sha256 of the content the user would trust.
        hash: String,
        /// The whole file text (lossy; invisible/bidi characters escaped, see
        /// [`escape_invisible`]).
        text: String,
        /// The text contained bidi or zero-width characters, shown escaped — warned about in
        /// the trust dialog.
        obfuscated: bool,
        /// Problems found while parsing (the menu itself is not offered).
        warnings: Vec<Warning>,
    },
    /// The file exists but cannot be used (unreadable, or changing while read).
    Error(String),
}

/// Everything one F2 needs from disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The active panel's folder.
    pub dir: PathBuf,
    /// The global menu.
    pub global: UserMenu,
    /// Problems of the global menu, the trust store and the first-use install.
    pub warnings: Vec<Warning>,
    /// The folder's local menu.
    pub local: LocalSource,
}

/// Read global menu, trust store and the local menu of `dir`; installs the default global menu
/// when none exists yet (never overwrites). Blocking I/O: call from a worker.
pub fn load_blocking(paths: &ConfigPaths, dir: &Path) -> Loaded {
    let mut warnings = Vec::new();
    if let Err(e) = install_default_global(paths) {
        warnings.push(Warning {
            file: Some(paths.menu_file()),
            message: format!("cannot create the default menu: {e}"),
        });
    }
    let (global, mut w) = UserMenu::load_global(paths);
    warnings.append(&mut w);
    let (store, mut w) = TrustStore::load(paths);
    warnings.append(&mut w);
    let local = load_local(&store, dir);
    Loaded {
        dir: dir.to_path_buf(),
        global,
        warnings,
        local,
    }
}

// Runs on the I/O pool (see `load_blocking`), never on the GTK thread.
#[allow(clippy::disallowed_methods)] // runs on the I/O pool, never on the GTK thread
fn load_local(store: &TrustStore, dir: &Path) -> LocalSource {
    let file = local_menu_path(dir);
    // The review text must be what the hash covers: read before and after the status check
    // and accept it only when both reads agree. Refuse files that cannot be fully shown.
    for _ in 0..3 {
        let before = match read_menu_file(&file) {
            Ok(b) => Some(b),
            Err(MenuReadError::Missing) => None,
            Err(MenuReadError::TooLarge) => {
                return LocalSource::Error(format!(
                    "{} is larger than {} KiB; not offering trust for a file that cannot be fully shown",
                    file.display(),
                    MAX_MENU_BYTES / 1024
                ));
            }
            Err(MenuReadError::Io(e)) => {
                return LocalSource::Error(format!("cannot read {}: {e}", file.display()));
            }
        };
        let status = match store.local_menu_status(dir) {
            Ok(s) => s,
            Err(e) => return LocalSource::Error(e.to_string()),
        };
        match status {
            LocalMenuStatus::Absent => return LocalSource::None,
            LocalMenuStatus::Trusted { menu, warnings } => {
                return LocalSource::Trusted { menu, warnings };
            }
            LocalMenuStatus::Untrusted { hash, menu: _ } => {
                let after = match read_menu_file(&file) {
                    Ok(b) => Some(b),
                    Err(MenuReadError::Missing) => None,
                    Err(_) => None, // changed under us: retry or fail below
                };
                let (Some(before), Some(after)) = (before, after) else {
                    continue;
                };
                if before != after {
                    continue;
                }
                let (text, obfuscated) = escape_invisible(&text_full(&after));
                let (_, warnings) = ofm_config::menu::parse_local(&text_full(&after), &file);
                return LocalSource::Untrusted {
                    hash,
                    text,
                    obfuscated,
                    warnings,
                };
            }
        }
    }
    LocalSource::Error(format!(
        "{} changed while it was being read",
        file.display()
    ))
}

/// Why a local menu file could not be read.
enum MenuReadError {
    /// The file does not exist.
    Missing,
    /// Larger than [`MAX_MENU_BYTES`].
    TooLarge,
    /// Any other I/O error (includes "not a regular file").
    Io(std::io::Error),
}

/// Read a local menu file without wedging the thread: opened `O_NONBLOCK`/`O_NOFOLLOW` and
/// fstat'ed, so a FIFO, device or symlink swapped in after the listing fails here instead of
/// blocking forever; at most [`MAX_MENU_BYTES`] are read and anything larger is refused.
#[allow(clippy::disallowed_methods)] // runs on the I/O pool, never on the GTK thread
fn read_menu_file(file: &Path) -> Result<Vec<u8>, MenuReadError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(file)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MenuReadError::Missing
            } else {
                MenuReadError::Io(e)
            }
        })?;
    let md = f.metadata().map_err(MenuReadError::Io)?;
    if !md.file_type().is_file() {
        return Err(MenuReadError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a regular file",
        )));
    }
    let mut buf = Vec::new();
    f.take(MAX_MENU_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(MenuReadError::Io)?;
    if buf.len() > MAX_MENU_BYTES {
        return Err(MenuReadError::TooLarge);
    }
    Ok(buf)
}

/// Escape characters that render invisibly or reorder text — bidi controls (U+202A-202E,
/// U+2066-2069), LRM/RLM, zero-width space/joiners (U+200B-200D), invisible operators
/// (U+2060-2064) and BOM — as visible `<U+XXXX>` markers, so a menu file cannot hide
/// commands from the trust review. Returns the escaped text and whether anything was escaped.
pub fn escape_invisible(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut hit = false;
    for c in text.chars() {
        let code = c as u32;
        if matches!(code, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x2069 | 0xFEFF)
        {
            hit = true;
            out.push_str(&format!("<U+{code:04X}>"));
        } else {
            out.push(c);
        }
    }
    (out, hit)
}

fn text_full(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Create `dir/.ofm-menu.toml` from [`LOCAL_TEMPLATE`] unless it exists (`create_new`: never
/// truncates or overwrites). Returns the path and whether it was created. Blocking I/O.
pub fn create_local_template(dir: &Path) -> std::io::Result<(PathBuf, bool)> {
    let file = local_menu_path(dir);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
    {
        Ok(mut f) => {
            f.write_all(LOCAL_TEMPLATE.as_bytes())?;
            f.sync_all()?;
            Ok((file, true))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok((file, false)),
        Err(e) => Err(e),
    }
}

/// One line per warning: `file: message`.
pub fn describe_warning(w: &Warning) -> String {
    match &w.file {
        Some(f) => format!("{}: {}", f.display(), w.message),
        None => w.message.clone(),
    }
}

/// An entry of the shown menu.
#[derive(Debug, Clone, PartialEq)]
pub struct ShownEntry {
    /// The menu item.
    pub item: MenuItem,
    /// Comes from the folder's `.ofm-menu.toml`.
    pub local: bool,
}

impl ShownEntry {
    /// The hotkey, upper-cased (empty or multi-char keys are no hotkey).
    pub fn hotkey(&self) -> Option<char> {
        let mut chars = self.item.key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c.to_uppercase().next().unwrap_or(c)),
            _ => None,
        }
    }
}

/// What the menu dialog shows.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuView {
    /// The active panel's folder (commands run here).
    pub dir: PathBuf,
    /// Local entries first, then global ones; masks applied.
    pub entries: Vec<ShownEntry>,
    /// Problem lines (menu files, trust store).
    pub problems: Vec<String>,
    /// The folder has an untrusted local menu that is not shown (the user chose "Ignore").
    pub local_ignored: bool,
}

/// Turn `loaded` into the shown entries for the cursor file `cursor`. An untrusted local menu
/// contributes nothing (`ignored` only records that the user already declined it, so the dialog
/// can offer a review).
pub fn build_view(loaded: &Loaded, cursor: Option<&OsStr>, ignored: bool) -> MenuView {
    let mut problems: Vec<String> = loaded.warnings.iter().map(describe_warning).collect();
    let mut local_items: Vec<MenuItem> = Vec::new();
    let mut local_ignored = false;
    match &loaded.local {
        LocalSource::None => {}
        LocalSource::Trusted { menu, warnings } => {
            local_items = menu.item.clone();
            problems.extend(warnings.iter().map(describe_warning));
        }
        LocalSource::Untrusted { .. } => local_ignored = ignored,
        LocalSource::Error(e) => problems.push(format!("local menu: {e}")),
    }
    let local_count = local_items.len();
    let merged = UserMenu::merged(
        Some(UserMenu {
            version: 1,
            item: local_items,
        }),
        loaded.global.clone(),
    );
    let entries = merged
        .item
        .into_iter()
        .enumerate()
        .filter(|(_, item)| !item.command.is_empty())
        .filter(|(_, item)| mask_allows(&item.mask, cursor))
        .map(|(i, item)| ShownEntry {
            item,
            local: i < local_count,
        })
        .collect();
    MenuView {
        dir: loaded.dir.clone(),
        entries,
        problems,
        local_ignored,
    }
}

/// An empty mask always shows the entry; otherwise the cursor file must match (no cursor file
/// or an invalid mask: hidden — the invalid mask is already reported as a warning).
pub fn mask_allows(mask: &str, cursor: Option<&OsStr>) -> bool {
    if mask.trim().is_empty() {
        return true;
    }
    let Some(name) = cursor else {
        return false;
    };
    Mask::parse(
        mask,
        MaskOpts {
            case_insensitive: true,
        },
    )
    .is_ok_and(|m| m.is_match(name))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn item(key: &str, title: &str, mask: &str) -> MenuItem {
        MenuItem {
            key: key.into(),
            title: title.into(),
            command: format!("echo {title}"),
            mask: mask.into(),
            ..MenuItem::default()
        }
    }

    fn loaded(global: Vec<MenuItem>, local: LocalSource) -> Loaded {
        Loaded {
            dir: PathBuf::from("/d"),
            global: UserMenu {
                version: 1,
                item: global,
            },
            warnings: Vec::new(),
            local,
        }
    }

    fn titles(v: &MenuView) -> Vec<&str> {
        v.entries.iter().map(|e| e.item.title.as_str()).collect()
    }

    #[test]
    fn local_entries_come_first_and_are_marked() {
        let l = loaded(
            vec![item("G", "global", "")],
            LocalSource::Trusted {
                menu: UserMenu {
                    version: 1,
                    item: vec![item("L", "local", "")],
                },
                warnings: Vec::new(),
            },
        );
        let v = build_view(&l, None, false);
        assert_eq!(titles(&v), ["local", "global"]);
        assert!(v.entries[0].local && !v.entries[1].local);
    }

    #[test]
    fn untrusted_local_menu_is_never_shown() {
        let l = loaded(
            vec![item("G", "global", "")],
            LocalSource::Untrusted {
                hash: "0".repeat(64),
                text: "x".into(),
                obfuscated: false,
                warnings: Vec::new(),
            },
        );
        assert_eq!(titles(&build_view(&l, None, false)), ["global"]);
        let v = build_view(&l, None, true);
        assert_eq!(titles(&v), ["global"]);
        assert!(v.local_ignored);
    }

    #[test]
    fn masks_filter_on_the_cursor_file() {
        let l = loaded(
            vec![item("P", "pics", "*.{jpg,png}"), item("A", "all", "")],
            LocalSource::None,
        );
        let name = |s: &'static str| Some(OsStr::new(s));
        assert_eq!(
            titles(&build_view(&l, name("x.PNG"), false)),
            ["pics", "all"]
        );
        assert_eq!(titles(&build_view(&l, name("x.txt"), false)), ["all"]);
        // No file under the cursor: masked entries are hidden.
        assert_eq!(titles(&build_view(&l, None, false)), ["all"]);
    }

    #[test]
    fn entries_without_a_command_are_dropped_and_problems_collected() {
        let mut l = loaded(
            vec![
                MenuItem {
                    title: "empty".into(),
                    ..MenuItem::default()
                },
                item("A", "ok", ""),
            ],
            LocalSource::Error("unreadable".into()),
        );
        l.warnings.push(Warning {
            file: Some(PathBuf::from("/c/menu.toml")),
            message: "bad".into(),
        });
        let v = build_view(&l, None, false);
        assert_eq!(titles(&v), ["ok"]);
        assert_eq!(v.problems.len(), 2);
        assert!(v.problems[0].starts_with("/c/menu.toml: bad"));
    }

    #[test]
    fn hotkey_is_one_char_upper_cased() {
        let e = |k: &str| ShownEntry {
            item: item(k, "t", ""),
            local: false,
        };
        assert_eq!(e("t").hotkey(), Some('T'));
        assert_eq!(e("").hotkey(), None);
        assert_eq!(e("ab").hotkey(), None);
    }

    #[test]
    fn local_template_is_created_once_and_never_overwritten() {
        let dir = ofm_testkit::temp::TempDir::new().unwrap();
        let (file, created) = create_local_template(dir.path()).unwrap();
        assert!(created);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), LOCAL_TEMPLATE);
        std::fs::write(&file, "mine").unwrap();
        let (_, created) = create_local_template(dir.path()).unwrap();
        assert!(!created);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    }

    #[test]
    fn template_parses_to_an_empty_valid_menu() {
        let (menu, warnings) = ofm_config::menu::parse_local(LOCAL_TEMPLATE, Path::new("t"));
        assert!(menu.item.is_empty());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn escape_invisible_marks_bidi_and_zero_width() {
        let (clean, hit) = escape_invisible("version = 1\ntitle = \"Build\"\n");
        assert!(!hit);
        assert_eq!(clean, "version = 1\ntitle = \"Build\"\n");
        // A command hidden behind a bidi override / zero-width characters becomes visible.
        let nasty = "title = \"x\u{202E}\"\u{200B}command = \"rm -rf ~\"\u{2066}";
        let (shown, hit) = escape_invisible(nasty);
        assert!(hit);
        assert!(shown.contains("<U+202E>"));
        assert!(shown.contains("<U+200B>"));
        assert!(shown.contains("<U+2066>"));
        assert!(shown.contains("command = \"rm -rf ~\""));
        assert!(!shown.contains('\u{202E}'));
    }

    #[test]
    fn oversized_local_menu_is_refused_not_partially_trusted() {
        let tmp = ofm_testkit::temp::TempDir::new().unwrap();
        let paths = ConfigPaths::with_dirs(tmp.path().join("config"), tmp.path().join("state"));
        let dir = tmp.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        // Just under the limit: fully shown (the trust hash covers everything displayed).
        let mut ok_body = String::from("version = 1\n");
        ok_body.push_str(&"#".repeat(MAX_MENU_BYTES - ok_body.len() - 1));
        ok_body.push('\n');
        assert_eq!(ok_body.len(), MAX_MENU_BYTES);
        std::fs::write(local_menu_path(&dir), &ok_body).unwrap();
        let LocalSource::Untrusted { text, .. } = load_blocking(&paths, &dir).local else {
            panic!("expected untrusted");
        };
        assert_eq!(text.len(), ok_body.len());

        // Over the limit: refused outright (no trust offered for invisible content).
        let big = std::fs::OpenOptions::new()
            .append(true)
            .open(local_menu_path(&dir))
            .unwrap();
        use std::io::Write as _;
        let mut big = big;
        big.write_all(&vec![b'#'; MAX_MENU_BYTES]).unwrap();
        drop(big);
        let LocalSource::Error(msg) = load_blocking(&paths, &dir).local else {
            panic!("expected refusal");
        };
        assert!(msg.contains("KiB"), "{msg}");
    }

    #[cfg(unix)]
    #[test]
    fn fifo_local_menu_is_refused_without_blocking() {
        let tmp = ofm_testkit::temp::TempDir::new().unwrap();
        let paths = ConfigPaths::with_dirs(tmp.path().join("config"), tmp.path().join("state"));
        let dir = tmp.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        let file = local_menu_path(&dir);
        let st = std::process::Command::new("mkfifo")
            .arg(&file)
            .status()
            .unwrap();
        assert!(st.success());
        // Must return promptly (O_NONBLOCK open + fstat reject); a blocking read would hang.
        assert!(matches!(
            load_blocking(&paths, &dir).local,
            LocalSource::Error(_)
        ));
    }

    #[test]
    fn untrusted_text_flags_obfuscated_characters() {
        let tmp = ofm_testkit::temp::TempDir::new().unwrap();
        let paths = ConfigPaths::with_dirs(tmp.path().join("config"), tmp.path().join("state"));
        let dir = tmp.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            local_menu_path(&dir),
            "version = 1\n# hidden \u{202E}stuff\n",
        )
        .unwrap();
        let LocalSource::Untrusted {
            text, obfuscated, ..
        } = load_blocking(&paths, &dir).local
        else {
            panic!("expected untrusted");
        };
        assert!(obfuscated);
        assert!(text.contains("<U+202E>"));
    }

    #[test]
    fn load_blocking_installs_defaults_and_classifies_the_local_menu() {
        let tmp = ofm_testkit::temp::TempDir::new().unwrap();
        let paths = ConfigPaths::with_dirs(tmp.path().join("config"), tmp.path().join("state"));
        let dir = tmp.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        let l = load_blocking(&paths, &dir);
        assert!(paths.menu_file().exists(), "default menu installed");
        assert!(!l.global.item.is_empty());
        assert_eq!(l.local, LocalSource::None);

        // A user's own menu is never overwritten by the install step.
        std::fs::write(paths.menu_file(), "version = 1\n").unwrap();
        let l = load_blocking(&paths, &dir);
        assert!(l.global.item.is_empty());
        assert_eq!(
            std::fs::read_to_string(paths.menu_file()).unwrap(),
            "version = 1\n"
        );

        // Local menu: untrusted -> trusted -> modified = untrusted again.
        let file = local_menu_path(&dir);
        let body = "version = 1\n[[item]]\nkey = \"X\"\ntitle = \"x\"\ncommand = \"true\"\n";
        std::fs::write(&file, body).unwrap();
        let LocalSource::Untrusted { hash, text, .. } = load_blocking(&paths, &dir).local else {
            panic!("expected untrusted");
        };
        assert_eq!(text, body);
        let (mut store, _) = TrustStore::load(&paths);
        store.trust(&dir, &hash).unwrap();
        assert!(matches!(
            load_blocking(&paths, &dir).local,
            LocalSource::Trusted { .. }
        ));
        std::fs::write(&file, format!("{body}# changed\n")).unwrap();
        assert!(matches!(
            load_blocking(&paths, &dir).local,
            LocalSource::Untrusted { .. }
        ));
    }
}
