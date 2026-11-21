//! Drag and drop onto a panel (F31). Dragging OUT of a panel is the file list's own drag source
//! (a `GdkFileList` of the marked files, accepted by GNOME Files and other apps); this module is
//! the drop side: files from other applications and from the other panel.
//!
//! * Drop on a panel = copy into its folder, Shift held = move.
//! * Drop on a folder row = copy/move into that folder (the `..` row drops into the parent).
//! * The panel (or the folder row) gets a highlight drawn from the theme palette while a drop
//!   would be accepted; nothing is highlighted where the drop would do nothing (a move onto the
//!   folder the items are already in).
//! * The transfer itself is an ordinary copy/move job (`transfer::submit`), so progress and
//!   conflict questions are the usual ones.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gdk::{self, DragAction, ModifierType};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use ofm_core::{Entry, Location};

use super::transfer;
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::theme::Palette;
use crate::widgets::file_list::{FileList, RowRect};
use crate::window::PanelView;

/// The folder a drop on `entry` (a row of the panel showing `here`) goes into: the folder itself,
/// the parent for the `..` row. `None` for files and for archives (read-only).
pub fn dest_for_row(here: &Location, entry: &Entry) -> Option<PathBuf> {
    if !entry.kind.is_dir_like() {
        return None;
    }
    match (here, &entry.real_path) {
        (Location::List { .. }, Some(real)) => Some(real.clone()),
        (Location::Local(dir), _) if entry.name == ".." => dir.parent().map(PathBuf::from),
        (Location::Local(dir), _) => Some(dir.join(&entry.name)),
        _ => None,
    }
}

/// The action to offer: move with Shift (when the source allows it), else copy, else whatever the
/// source allows. `DragAction::empty()` when the source allows neither.
pub fn choose_action(offered: DragAction, shift: bool) -> DragAction {
    let can = offered & (DragAction::COPY | DragAction::MOVE);
    if shift && can.contains(DragAction::MOVE) {
        DragAction::MOVE
    } else if can.contains(DragAction::COPY) {
        DragAction::COPY
    } else if can.contains(DragAction::MOVE) {
        DragAction::MOVE
    } else {
        DragAction::empty()
    }
}

/// CSS of the highlight widgets, from the theme palette (the accent is the cursor bar colour).
pub fn style_css(palette: &Palette) -> String {
    let c = palette.cursor.active_bg;
    let rgb = |a: f32| {
        format!(
            "rgba({}, {}, {}, {a:.2})",
            (c.red() * 255.0).round() as u8,
            (c.green() * 255.0).round() as u8,
            (c.blue() * 255.0).round() as u8
        )
    };
    format!(
        ".ofm-drop-frame {{ border: 3px solid {solid}; background-color: {wash}; }}\n\
         .ofm-drop-row {{ border: 2px solid {solid}; background-color: {tint}; }}\n",
        solid = rgb(1.0),
        tint = rgb(0.30),
        wash = rgb(0.14),
    )
}

/// Drop-UI state in `ctx.ext`: the style provider (kept alive) and every panel's highlight.
struct DropUi {
    _provider: gtk::CssProvider,
    highlights: RefCell<Vec<(usize, Rc<Highlight>)>>,
}

fn ensure_style(ctx: &Rc<AppContext>) -> Rc<DropUi> {
    if let Some(ui) = ctx.ext.get::<DropUi>() {
        return ui;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&style_css(&ctx.theme.palette()));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let p2 = provider.clone();
    ctx.theme
        .changed
        .connect(move |palette| p2.load_from_string(&style_css(palette)));
    let ui = Rc::new(DropUi {
        _provider: provider,
        highlights: RefCell::new(Vec::new()),
    });
    ctx.ext.insert(Rc::clone(&ui));
    ui
}

/// The highlight widgets of one panel (children of its overlay).
struct Highlight {
    frame: gtk::Box,
    row: gtk::Box,
}

impl Highlight {
    fn new(overlay: &gtk::Overlay) -> Highlight {
        let make = |class: &str, name: &str| {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            b.add_css_class(class);
            b.set_widget_name(name);
            b.set_can_target(false);
            b.set_can_focus(false);
            b.set_visible(false);
            overlay.add_overlay(&b);
            overlay.set_measure_overlay(&b, false);
            overlay.set_clip_overlay(&b, true);
            b
        };
        let frame = make("ofm-drop-frame", "ofm-drop-frame");
        frame.set_halign(gtk::Align::Fill);
        frame.set_valign(gtk::Align::Fill);
        let row = make("ofm-drop-row", "ofm-drop-row");
        row.set_halign(gtk::Align::Start);
        row.set_valign(gtk::Align::Start);
        Highlight { frame, row }
    }

    fn show_panel(&self) {
        self.row.set_visible(false);
        self.frame.set_visible(true);
    }

    fn show_row(&self, r: &RowRect) {
        self.frame.set_visible(false);
        self.row.set_margin_start(r.x.round() as i32);
        self.row.set_margin_top(r.y.round() as i32);
        self.row
            .set_size_request(r.width.round() as i32, r.height.round() as i32);
        self.row.set_visible(true);
    }

    fn clear(&self) {
        self.frame.set_visible(false);
        self.row.set_visible(false);
    }
}

/// Where a drop at (x, y) of `list` (showing the active tab of `side`) would go, plus the row to
/// highlight when it is a folder row.
fn dest_at(
    ctx: &AppContext,
    side: usize,
    list: &FileList,
    x: f64,
    y: f64,
) -> Option<(PathBuf, Option<RowRect>)> {
    let ctrl = ctx.host(side).active();
    let here = ctrl.location();
    if let Some(r) = list.row_at(x, y) {
        let dest = {
            let model = ctrl.model();
            model.visible(r.index).and_then(|e| dest_for_row(&here, e))
        };
        if let Some(dest) = dest {
            return Some((dest, Some(r)));
        }
    }
    match here {
        Location::Local(p) => Some((p, None)),
        _ => None,
    }
}

fn shift_held(target: &gtk::DropTarget) -> bool {
    target.current_drop().is_some_and(|d| {
        d.device()
            .modifier_state()
            .contains(ModifierType::SHIFT_MASK)
    })
}

fn offered(target: &gtk::DropTarget) -> DragAction {
    target
        .current_drop()
        .map_or(DragAction::COPY | DragAction::MOVE, |d| d.actions())
}

/// Local paths of a dropped/preloaded value (non-local files are skipped).
fn paths_of(value: &glib::Value) -> Vec<PathBuf> {
    value
        .get::<gdk::FileList>()
        .map(|l| l.files().iter().filter_map(gio::File::path).collect())
        .unwrap_or_default()
}

/// Make `view`'s file list accept file drops (hook: `desktop::install_panel_view`).
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    let drop_ui = ensure_style(ctx);
    let highlight = Rc::new(Highlight::new(&view.overlay));
    let target = gtk::DropTarget::new(
        gdk::FileList::static_type(),
        DragAction::COPY | DragAction::MOVE,
    );
    // Preload: the dragged paths are known while moving, so a drop that would do nothing is not
    // offered.
    target.set_preload(true);
    let (side, list) = (view.side, view.list.clone());
    // The action returned by the last motion (the drop must not run a job the pointer never
    // showed).
    let last = Rc::new(Cell::new(DragAction::empty()));
    let ctx_weak = Rc::downgrade(ctx);

    {
        let (ctx_weak, list, highlight, last) = (
            ctx_weak.clone(),
            list.clone(),
            Rc::clone(&highlight),
            Rc::clone(&last),
        );
        target.connect_motion(move |t, x, y| {
            let Some(ctx) = ctx_weak.upgrade() else {
                return DragAction::empty();
            };
            let action = choose_action(offered(t), shift_held(t));
            let refuse = |highlight: &Highlight| {
                highlight.clear();
                last.set(DragAction::empty());
                DragAction::empty()
            };
            if action.is_empty() {
                return refuse(&highlight);
            }
            let Some((dest, row)) = dest_at(&ctx, side, &list, x, y) else {
                return refuse(&highlight);
            };
            if let Some(v) = t.value() {
                let paths = paths_of(&v);
                let mv = action == DragAction::MOVE;
                if !paths.is_empty() && transfer::effective_sources(&paths, &dest, mv).is_empty() {
                    return refuse(&highlight);
                }
            }
            match row {
                Some(r) => highlight.show_row(&r),
                None => highlight.show_panel(),
            }
            last.set(action);
            action
        });
    }
    {
        let highlight = Rc::clone(&highlight);
        target.connect_leave(move |_| highlight.clear());
    }
    {
        let (highlight, list) = (Rc::clone(&highlight), list.clone());
        target.connect_drop(move |t, value, x, y| {
            highlight.clear();
            let Some(ctx) = ctx_weak.upgrade() else {
                return false;
            };
            let action = choose_action(offered(t), shift_held(t));
            if action.is_empty() || last.get().is_empty() {
                return false;
            }
            let Some((dest, _)) = dest_at(&ctx, side, &list, x, y) else {
                return false;
            };
            let paths = paths_of(value);
            if paths.is_empty() {
                ui::message(
                    &ctx,
                    tr("Drop"),
                    tr("Only files and folders on this computer can be dropped here."),
                );
                return false;
            }
            transfer::submit(&ctx, &paths, &dest, action == DragAction::MOVE)
        });
    }
    list.add_controller(target);
    // Keep the highlight widgets reachable for tests and screenshots.
    drop_ui.highlights.borrow_mut().push((side, highlight));
}

/// Show the panel (or folder row) highlight of `side` as a drop would (screenshots, tests).
/// `row`: highlight that row rectangle instead of the whole panel.
pub fn preview_highlight(ctx: &AppContext, side: usize, row: Option<RowRect>) {
    let Some(ui) = ctx.ext.get::<DropUi>() else {
        return;
    };
    for (_, hl) in ui.highlights.borrow().iter().filter(|(s, _)| *s == side) {
        match &row {
            Some(r) => hl.show_row(r),
            None => hl.show_panel(),
        }
    }
}

/// Hide every highlight of `side`.
pub fn clear_highlight(ctx: &AppContext, side: usize) {
    let Some(ui) = ctx.ext.get::<DropUi>() else {
        return;
    };
    for (_, hl) in ui.highlights.borrow().iter().filter(|(s, _)| *s == side) {
        hl.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_core::{EntryKind, ListId};

    use super::*;

    fn dir(name: &str) -> Entry {
        Entry::new(name, EntryKind::Dir)
    }

    #[test]
    fn folder_rows_are_drop_destinations() {
        let here = Location::Local("/a/b".into());
        assert_eq!(
            dest_for_row(&here, &dir("c")),
            Some(PathBuf::from("/a/b/c"))
        );
        assert_eq!(dest_for_row(&here, &dir("..")), Some(PathBuf::from("/a")));
        assert_eq!(dest_for_row(&here, &Entry::new("f", EntryKind::File)), None);
        let root = Location::Local("/".into());
        assert_eq!(dest_for_row(&root, &dir("..")), None);
    }

    #[test]
    fn list_rows_use_the_real_path_and_archives_refuse() {
        let list = Location::List { id: ListId(1) };
        let mut d = dir("proj");
        d.real_path = Some("/real/proj".into());
        assert_eq!(dest_for_row(&list, &d), Some(PathBuf::from("/real/proj")));
        let archive = Location::Archive {
            archive: "/x.zip".into(),
            inner: "".into(),
        };
        assert_eq!(dest_for_row(&archive, &dir("sub")), None);
    }

    #[test]
    fn action_choice() {
        let both = DragAction::COPY | DragAction::MOVE | DragAction::ASK;
        assert_eq!(choose_action(both, false), DragAction::COPY);
        assert_eq!(choose_action(both, true), DragAction::MOVE);
        assert_eq!(choose_action(DragAction::COPY, true), DragAction::COPY);
        assert_eq!(choose_action(DragAction::MOVE, false), DragAction::MOVE);
        assert_eq!(choose_action(DragAction::LINK, false), DragAction::empty());
        assert_eq!(
            choose_action(DragAction::empty(), true),
            DragAction::empty()
        );
    }
}
