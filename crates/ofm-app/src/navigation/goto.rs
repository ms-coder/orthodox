//! Moving around (F09): go up / root / home / path, open, enter, refresh, swap panels, same dir.
//! All navigation funnels through [`navigate`], which also implements the tab-lock rule.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use ofm_core::{Location, cmd};

use super::modal;
use super::quicksearch;
use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::panel::{LoadOpts, PanelController, PanelHost};

/// Register the commands of this module.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_GO_UP, go_up);
    reg.register(cmd::PANEL_GO_ROOT, go_root);
    reg.register(cmd::PANEL_GO_HOME, go_home);
    reg.register(cmd::PANEL_GOTO_PATH, goto_path_dialog);
    reg.register(cmd::PANEL_OPEN, |ctx| open_cursor(ctx, true));
    reg.register(cmd::PANEL_ENTER_DIR, |ctx| open_cursor(ctx, false));
    reg.register(cmd::PANEL_REFRESH, |ctx| ctx.active_controller().reload());
    reg.register(cmd::PANEL_SWAP, swap_panels);
    reg.register(cmd::PANEL_SAME_DIR, same_dir);
}

// ---- pure helpers (unit-tested) ------------------------------------------------------------------

/// Parent of `loc` plus the name of the entry we leave (where the cursor should land).
/// `None` at the top (file system root, panelized list).
pub fn parent_of(loc: &Location) -> Option<(Location, OsString)> {
    match loc {
        Location::Local(p) => {
            let parent = p.parent()?;
            let name = p.file_name()?.to_owned();
            Some((Location::Local(parent.to_owned()), name))
        }
        Location::Archive { archive, inner } => {
            if let (Some(parent), Some(name)) = (inner.parent(), inner.file_name()) {
                Some((
                    Location::Archive {
                        archive: archive.clone(),
                        inner: parent.to_owned(),
                    },
                    name.to_owned(),
                ))
            } else {
                // Archive root: leave the archive, cursor on the archive file.
                let parent = archive.parent()?;
                let name = archive.file_name()?.to_owned();
                Some((Location::Local(parent.to_owned()), name))
            }
        }
        Location::List { .. } => None,
    }
}

/// Resolve text typed in the "go to path" dialog: `~` expansion, relative to `base`, lexical
/// cleanup of `.` and `..` (no file system access).
pub fn resolve_input(text: &str, base: &Path, home: &Path) -> PathBuf {
    let text = text.trim();
    let joined = if text == "~" {
        home.to_owned()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(text).is_absolute() {
        PathBuf::from(text)
    } else {
        base.join(text)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push("/");
    }
    out
}

// ---- navigation ----------------------------------------------------------------------------------

fn host_of<'a>(ctx: &'a AppContext, ctrl: &Rc<PanelController>) -> Option<&'a Rc<PanelHost>> {
    ctx.panels
        .iter()
        .find(|h| h.tabs().iter().any(|t| Rc::ptr_eq(t, ctrl)))
}

/// Navigate `ctrl` to `loc` (recorded in its history). A locked tab never leaves its directory:
/// the target opens in a new tab beside it instead. Navigating to the current location is a no-op.
pub fn navigate(
    ctx: &Rc<AppContext>,
    ctrl: &Rc<PanelController>,
    loc: Location,
    cursor_name: Option<OsString>,
) {
    if ctrl.location() == loc {
        return;
    }
    if ctrl.locked()
        && let Some(host) = host_of(ctx, ctrl)
    {
        let tab = host.add_tab(loc.clone(), true);
        if cursor_name.is_some() {
            tab.load(
                loc,
                LoadOpts {
                    cursor_name,
                    ..LoadOpts::default()
                },
            );
        }
        return;
    }
    ctrl.load(
        loc,
        LoadOpts {
            keep_selection: false,
            cursor_name,
            skip_history: false,
        },
    );
}

fn go_up(ctx: &Rc<AppContext>) {
    let ctrl = ctx.active_controller();
    if let Some((parent, from)) = parent_of(&ctrl.location()) {
        navigate(ctx, &ctrl, parent, Some(from));
    }
}

fn go_root(ctx: &Rc<AppContext>) {
    let ctrl = ctx.active_controller();
    navigate(ctx, &ctrl, Location::Local(PathBuf::from("/")), None);
}

fn go_home(ctx: &Rc<AppContext>) {
    let ctrl = ctx.active_controller();
    navigate(ctx, &ctrl, Location::Local(gtk::glib::home_dir()), None);
}

/// Enter the entry under the cursor. `run`: files are opened with the default application
/// (`panel.open`); `panel.enter_dir` never runs anything.
fn open_cursor(ctx: &Rc<AppContext>, run: bool) {
    // Enter confirms an open quick search / filter first.
    quicksearch::confirm_active(ctx);
    let ctrl = ctx.active_controller();
    let Some(entry) = ctrl.model().cursor_entry().cloned() else {
        return;
    };
    let here = ctrl.location();
    if entry.name == ".." {
        go_up(ctx);
        return;
    }
    if entry.kind.is_dir_like() {
        let target = match (&here, &entry.real_path) {
            (Location::List { .. }, Some(real)) => Location::Local(real.clone()),
            _ => here.join(&entry.name),
        };
        navigate(ctx, &ctrl, target, None);
        return;
    }
    // FIFOs, devices and sockets are never "opened": Enter on one would block a pool
    // thread forever (desktop::open::probe used to open them unconditionally).
    if entry.kind == ofm_core::EntryKind::Other {
        ui::message(ctx, tr("Open"), tr("Special file (not a regular file)."));
        return;
    }
    if !run {
        return;
    }
    let file = match (&here, &entry.real_path) {
        (Location::Local(dir), _) => Some(dir.join(&entry.name)),
        (Location::List { .. }, Some(real)) => Some(real.clone()),
        _ => None,
    };
    match file {
        Some(path) => crate::desktop::open::open_path(ctx, path),
        None => tracing::debug!(name = ?entry.name, "open: not a local file"),
    }
}

fn swap_panels(ctx: &Rc<AppContext>) {
    let (a, b) = (ctx.active_controller(), ctx.other_controller());
    let (la, lb) = (a.location(), b.location());
    let (ca, cb) = (
        a.model().cursor_entry().map(|e| e.name.clone()),
        b.model().cursor_entry().map(|e| e.name.clone()),
    );
    navigate(ctx, &a, lb, cb);
    navigate(ctx, &b, la, ca);
}

fn same_dir(ctx: &Rc<AppContext>) {
    let here = ctx.active_controller().location();
    let other = ctx.other_controller();
    navigate(ctx, &other, here, None);
}

// ---- go to path dialog -----------------------------------------------------------------------------

fn goto_path_dialog(ctx: &Rc<AppContext>) {
    let ctrl = ctx.active_controller();
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Go to path"));
    dialog.set_content_width(520);
    let entry = gtk::Entry::new();
    entry.set_text(&match ctrl.location() {
        Location::Local(p) => p.to_string_lossy().into_owned(),
        other => other.display_lossy(),
    });
    entry.set_activates_default(false);
    entry.update_property(&[gtk::accessible::Property::Label(tr("Path"))]);
    let column = modal::padded_column(8);
    column.append(
        &gtk::Label::builder()
            .label(tr("Go to directory:"))
            .xalign(0.0)
            .build(),
    );
    column.append(&entry);
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));
    let ctx2 = Rc::clone(ctx);
    let entry2 = entry.clone();
    modal::present(ctx, &dialog, move || {
        let text = entry2.text();
        if text.trim().is_empty() {
            return false;
        }
        let base = match ctrl.location() {
            Location::Local(p) => p,
            Location::Archive { archive, .. } => {
                archive.parent().map(Path::to_owned).unwrap_or_default()
            }
            Location::List { .. } => PathBuf::from("/"),
        };
        let target = resolve_input(&text, &base, &gtk::glib::home_dir());
        navigate(&ctx2, &ctrl, Location::Local(target), None);
        true
    });
    entry.select_region(0, -1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(p: &str) -> Location {
        Location::Local(p.into())
    }

    #[test]
    fn parent_of_local() {
        assert_eq!(
            parent_of(&local("/a/b")),
            Some((local("/a"), OsString::from("b")))
        );
        assert_eq!(
            parent_of(&local("/a")),
            Some((local("/"), OsString::from("a")))
        );
        assert_eq!(parent_of(&local("/")), None);
    }

    #[test]
    fn parent_of_archive_leaves_to_archive_file() {
        let at = |inner: &str| Location::Archive {
            archive: "/d/x.zip".into(),
            inner: inner.into(),
        };
        assert_eq!(parent_of(&at("a/b")), Some((at("a"), OsString::from("b"))));
        assert_eq!(parent_of(&at("a")), Some((at(""), OsString::from("a"))));
        assert_eq!(
            parent_of(&at("")),
            Some((local("/d"), OsString::from("x.zip")))
        );
        assert_eq!(
            parent_of(&Location::List {
                id: ofm_core::ListId(1)
            }),
            None
        );
    }

    #[test]
    fn resolve_input_rules() {
        let base = Path::new("/work/proj");
        let home = Path::new("/home/u");
        assert_eq!(resolve_input("~", base, home), Path::new("/home/u"));
        assert_eq!(resolve_input("~/x", base, home), Path::new("/home/u/x"));
        assert_eq!(resolve_input("/etc/", base, home), Path::new("/etc"));
        assert_eq!(
            resolve_input("sub/../a", base, home),
            Path::new("/work/proj/a")
        );
        assert_eq!(resolve_input("../..", base, home), Path::new("/"));
        assert_eq!(resolve_input("../../../..", base, home), Path::new("/"));
        assert_eq!(
            resolve_input("  ./x  ", base, home),
            Path::new("/work/proj/x")
        );
    }
}
