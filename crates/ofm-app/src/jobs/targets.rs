//! What a file command works on: the active panel's selection, or the item under the cursor.
//! Pure data (no file system access), so the dialogs can be tested without real files.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ofm_core::{Entry, EntryKind, Location};

use crate::context::AppContext;
use crate::panel::PanelController;

/// The items an operation applies to, plus the directory they live in.
#[derive(Clone)]
pub struct Targets {
    /// Location of the panel the items belong to.
    pub dir: Location,
    /// The items (never the synthetic `..` row), in view order.
    pub entries: Vec<Entry>,
    /// True when the items come from the selection (false: the item under the cursor).
    pub from_selection: bool,
    /// The panel they came from (selection is cleared there after a successful job).
    pub controller: Rc<PanelController>,
}

impl Targets {
    /// Targets of the active panel; `None` when there is nothing to act on (empty directory,
    /// cursor on `..`).
    pub fn from_active(ctx: &AppContext) -> Option<Targets> {
        Self::from_controller(ctx.active_controller())
    }

    /// Targets of `controller`.
    pub fn from_controller(controller: Rc<PanelController>) -> Option<Targets> {
        let dir = controller.location();
        let (entries, from_selection) = {
            let m = controller.model();
            let selected = m.selected_names();
            if selected.is_empty() {
                let cursor = m.cursor_entry().filter(|e| !is_parent(&e.name)).cloned();
                (cursor.into_iter().collect::<Vec<_>>(), false)
            } else {
                let wanted: HashSet<&OsStr> = selected.iter().map(OsString::as_os_str).collect();
                let mut found: Vec<Entry> = (0..m.visible_len())
                    .filter_map(|i| m.visible(i))
                    .filter(|e| wanted.contains(e.name.as_os_str()))
                    .cloned()
                    .collect();
                // Selected but not visible (filtered/hidden) or not streamed in yet: the
                // operation must still touch exactly what was selected.
                let have: HashSet<OsString> = found.iter().map(|e| e.name.clone()).collect();
                for name in &selected {
                    if !have.contains(name) && !is_parent(name) {
                        found.push(Entry::new(name.clone(), EntryKind::File));
                    }
                }
                (found, true)
            }
        };
        if entries.is_empty() {
            return None;
        }
        Some(Targets {
            dir,
            entries,
            from_selection,
            controller,
        })
    }

    /// Directory of the panel when it is a plain local directory.
    pub fn local_dir(&self) -> Option<&Path> {
        self.dir.local_path()
    }

    /// Item names in order.
    pub fn names(&self) -> Vec<OsString> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// Sources for copy/move: local paths for local and panelized (list) panels, archive
    /// locations for archives (the engine reports those as unsupported until M6).
    pub fn sources(&self) -> Vec<Location> {
        self.entries
            .iter()
            .map(|e| match (&self.dir, &e.real_path) {
                (Location::List { .. }, Some(p)) => Location::Local(p.clone()),
                (dir, _) => dir.join(&e.name),
            })
            .collect()
    }

    /// Real local paths of every item (delete, attributes, size), `None` if any item is not on
    /// the local file system (archive entries are read-only).
    pub fn local_paths(&self) -> Option<Vec<PathBuf>> {
        self.sources()
            .into_iter()
            .map(|l| match l {
                Location::Local(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// True if any item is a directory (not a symlink to one).
    pub fn has_dirs(&self) -> bool {
        self.entries.iter().any(|e| e.kind == EntryKind::Dir)
    }

    /// `'name'` for a single item, `N items` otherwise (display only, lossy).
    pub fn describe(&self) -> String {
        describe_names(self.entries.iter().map(|e| e.name.as_os_str()))
    }
}

/// True for the synthetic parent row.
fn is_parent(name: &OsStr) -> bool {
    name == ".."
}

/// `'name'` for one item, `N items` otherwise.
pub fn describe_names<'a>(names: impl ExactSizeIterator<Item = &'a OsStr>) -> String {
    let mut names = names;
    if names.len() == 1 {
        let one = names.next().map(|n| n.to_string_lossy().into_owned());
        return format!("'{}'", one.unwrap_or_default());
    }
    format!("{} items", names.len())
}

/// Resolve user text typed into a path field: `~` and `~/x` expand to the home directory,
/// relative paths are taken relative to `base`, `.`/`..` are resolved lexically (no file system
/// access). Returns `None` for empty text.
pub fn resolve_path(text: &str, base: &Path, home: &Path) -> Option<PathBuf> {
    let text = text.trim();
    if text.is_empty() || text.contains('\0') {
        return None;
    }
    let joined = if text == "~" {
        home.to_owned()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(text).is_absolute() {
        PathBuf::from(text)
    } else {
        base.join(text)
    };
    Some(normalize(&joined))
}

/// Like [`resolve_path`] for a field whose initial text was the lossy rendering of `origin`
/// (invariant 4: paths are bytes). When `origin` is not valid UTF-8 and the text still starts with
/// that rendering (unchanged, or only extended below it with `/more`), the ORIGINAL bytes are
/// used for that part instead of the text with U+FFFD in it. Text the user changed elsewhere is
/// taken as typed (UTF-8).
pub fn resolve_path_with_origin(
    text: &str,
    origin: Option<&Path>,
    base: &Path,
    home: &Path,
) -> Option<PathBuf> {
    if let Some(origin) = origin
        && origin.to_str().is_none()
    {
        let lossy = origin.to_string_lossy();
        let trimmed = text.trim();
        if !trimmed.contains('\0') {
            if trimmed == lossy {
                return Some(normalize(origin));
            }
            if let Some(rest) = trimmed
                .strip_prefix(lossy.as_ref())
                .and_then(|r| r.strip_prefix('/'))
            {
                return Some(normalize(&origin.join(rest)));
            }
        }
    }
    resolve_path(text, base, home)
}

/// Lexical `.`/`..` removal (never touches the file system, so symlinks are not resolved).
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Which name in a panel showing `panel_dir` should receive the cursor after a job created
/// `created`: the created item itself when it is directly inside, or the top-level directory
/// that contains it (`mkdir a/b/c` -> `a`).
pub fn focus_target(panel_dir: &Path, created: &Path) -> Option<OsString> {
    let rel = created.strip_prefix(panel_dir).ok()?;
    rel.components().next().map(|c| c.as_os_str().to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_home_and_dots() {
        let base = Path::new("/work/proj");
        let home = Path::new("/home/me");
        assert_eq!(
            resolve_path("x/y", base, home),
            Some(PathBuf::from("/work/proj/x/y"))
        );
        assert_eq!(
            resolve_path("~/d", base, home),
            Some(PathBuf::from("/home/me/d"))
        );
        assert_eq!(
            resolve_path("~", base, home),
            Some(PathBuf::from("/home/me"))
        );
        assert_eq!(
            resolve_path("../a/./b", base, home),
            Some(PathBuf::from("/work/a/b"))
        );
        assert_eq!(
            resolve_path("/tmp/x/", base, home),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(resolve_path("   ", base, home), None);
        assert_eq!(resolve_path("a\0b", base, home), None);
    }

    #[test]
    fn non_utf8_origin_keeps_its_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let origin = PathBuf::from(std::ffi::OsStr::from_bytes(b"/data/caf\xe9 dir"));
        let (base, home) = (Path::new("/w"), Path::new("/h"));
        let lossy = origin.to_string_lossy().into_owned();
        assert!(lossy.contains('\u{fffd}'));
        let same = resolve_path_with_origin(&lossy, Some(&origin), base, home);
        assert_eq!(same.as_deref(), Some(origin.as_path()));
        let trailing = resolve_path_with_origin(&format!("{lossy}/"), Some(&origin), base, home);
        assert_eq!(trailing.as_deref(), Some(origin.as_path()));
        let below = resolve_path_with_origin(&format!("{lossy}/sub"), Some(&origin), base, home);
        assert_eq!(below, Some(origin.join("sub")));
        // Edited text is taken as typed.
        let edited = resolve_path_with_origin("/other", Some(&origin), base, home);
        assert_eq!(edited, Some(PathBuf::from("/other")));
        let sibling = resolve_path_with_origin(&format!("{lossy}x"), Some(&origin), base, home);
        assert_eq!(sibling, Some(PathBuf::from(format!("{lossy}x"))));
        // A UTF-8 origin and no origin behave like resolve_path.
        let utf8 = Path::new("/a/b");
        assert_eq!(
            resolve_path_with_origin("/a/b", Some(utf8), base, home),
            Some(PathBuf::from("/a/b"))
        );
        assert_eq!(
            resolve_path_with_origin("  ", Some(&origin), base, home),
            None
        );
    }

    #[test]
    fn focus_target_picks_top_level_component() {
        let dir = Path::new("/p");
        assert_eq!(
            focus_target(dir, Path::new("/p/new.txt")),
            Some(OsString::from("new.txt"))
        );
        assert_eq!(
            focus_target(dir, Path::new("/p/a/b/c")),
            Some(OsString::from("a"))
        );
        assert_eq!(focus_target(dir, Path::new("/q/a")), None);
        assert_eq!(focus_target(dir, Path::new("/p")), None);
    }

    #[test]
    fn describes_one_and_many() {
        let one = [OsString::from("a b")];
        assert_eq!(describe_names(one.iter().map(|s| s.as_os_str())), "'a b'");
        let two = [OsString::from("a"), OsString::from("b")];
        assert_eq!(describe_names(two.iter().map(|s| s.as_os_str())), "2 items");
    }
}
