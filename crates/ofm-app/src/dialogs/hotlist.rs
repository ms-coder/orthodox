//! Directory hotlist popup (Ctrl+D) and editor, `panel.hotlist`, `panel.hotlist_add`.
//! Owner: T2.3 (F07). The list is stored in `history.toml` (`hotlist`), written in the background.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use ofm_config::history::Bookmark;
use ofm_core::{Location, cmd};

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::chooser::{self, ChoiceRow, ChooserButton, ChooserSpec};
use crate::navigation::goto::navigate;
use crate::navigation::modal;
use crate::navigation::store::HistoryStore;

/// Register the hotlist commands.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_HOTLIST, popup);
    reg.register(cmd::PANEL_HOTLIST_ADD, |ctx| {
        add_current(ctx);
    });
}

/// Make sure the history store (which holds the hotlist) exists.
pub fn install_state(ctx: &Rc<AppContext>) {
    HistoryStore::ensure(ctx);
}

/// Title for a directory: its last component, `/` for the root.
pub fn default_title(path: &std::path::Path) -> String {
    path.file_name()
        .map_or_else(|| "/".to_owned(), |n| n.to_string_lossy().into_owned())
}

/// Append the active panel's directory to the hotlist unless it is already there. Returns whether
/// an entry was added (false for non-local locations and duplicates).
pub fn add_current(ctx: &Rc<AppContext>) -> bool {
    let Location::Local(path) = ctx.active_controller().location() else {
        return false;
    };
    let store = HistoryStore::ensure(ctx);
    let mut added = false;
    store.update(|h| {
        if h.hotlist
            .iter()
            .any(|b| b.path_buf().as_ref() == Some(&path))
        {
            return;
        }
        let mut b = Bookmark {
            title: default_title(&path),
            ..Bookmark::default()
        };
        b.set_path(&path);
        h.hotlist.push(b);
        added = true;
    });
    added
}

fn popup(ctx: &Rc<AppContext>) {
    let items: Vec<(String, PathBuf)> = HistoryStore::ensure(ctx)
        .snapshot()
        .hotlist
        .into_iter()
        .filter_map(|b| b.path_buf().map(|p| (b.title, p)))
        .collect();
    let rows = items
        .iter()
        .map(|(title, path)| ChoiceRow {
            title: title.clone(),
            subtitle: Some(path.to_string_lossy().into_owned()),
        })
        .collect();
    let on_pick = Rc::new(move |ctx: &Rc<AppContext>, idx: usize| {
        if let Some((_, path)) = items.get(idx) {
            let ctrl = ctx.active_controller();
            navigate(ctx, &ctrl, Location::Local(path.clone()), None);
        }
    });
    chooser::open(
        ctx,
        ChooserSpec {
            title: "Hotlist",
            rows,
            selected: 0,
            empty_text: "The hotlist is empty. Add the current directory with Ctrl+Shift+D.",
            on_pick,
            buttons: vec![
                ChooserButton {
                    label: "_Add current directory",
                    on_click: Rc::new(|ctx, dialog| {
                        add_current(ctx);
                        dialog.close();
                        popup(ctx);
                    }),
                },
                ChooserButton {
                    label: "_Edit...",
                    on_click: Rc::new(|ctx, dialog| {
                        dialog.close();
                        edit(ctx);
                    }),
                },
            ],
        },
    );
}

// ---- editor -----------------------------------------------------------------------------------

struct Editor {
    items: RefCell<Vec<Bookmark>>,
    list: gtk::ListBox,
    title: gtk::Entry,
    path: gtk::Entry,
    /// True while the entries are filled from code (not typed).
    loading: Cell<bool>,
}

impl Editor {
    fn selected(&self) -> Option<usize> {
        self.list
            .selected_row()
            .and_then(|r| usize::try_from(r.index()).ok())
    }

    fn row_texts(b: &Bookmark) -> (String, String) {
        let path = b.path_buf().map(|p| p.to_string_lossy().into_owned());
        (b.title.clone(), path.unwrap_or_default())
    }

    fn fill_row(row: &adw::ActionRow, b: &Bookmark) {
        let (t, p) = Self::row_texts(b);
        row.set_title(&t);
        row.set_subtitle(&p);
    }

    fn rebuild(&self, select: Option<usize>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for b in self.items.borrow().iter() {
            let row = adw::ActionRow::new();
            row.set_use_markup(false);
            Self::fill_row(&row, b);
            self.list.append(&row);
        }
        let n = self.items.borrow().len();
        let pick = select.map(|i| i.min(n.saturating_sub(1)));
        if let Some(row) = pick.and_then(|i| self.list.row_at_index(i.try_into().unwrap_or(0))) {
            self.list.select_row(Some(&row));
        }
        self.load_selected();
    }

    fn load_selected(&self) {
        self.loading.set(true);
        let (t, p) = match self
            .selected()
            .and_then(|i| self.items.borrow().get(i).cloned())
        {
            Some(b) => Self::row_texts(&b),
            None => (String::new(), String::new()),
        };
        self.title.set_text(&t);
        self.path.set_text(&p);
        self.loading.set(false);
        let has = self.selected().is_some();
        self.title.set_sensitive(has);
        self.path.set_sensitive(has);
    }

    /// Entry text changed: write it into the selected bookmark and its row.
    fn edited(&self) {
        if self.loading.get() {
            return;
        }
        let Some(i) = self.selected() else { return };
        {
            let mut items = self.items.borrow_mut();
            let Some(b) = items.get_mut(i) else { return };
            b.title = self.title.text().to_string();
            b.set_path(std::path::Path::new(self.path.text().as_str()));
        }
        let items = self.items.borrow();
        if let (Some(b), Some(row)) = (items.get(i), self.list.row_at_index(i as i32))
            && let Ok(row) = row.downcast::<adw::ActionRow>()
        {
            Self::fill_row(&row, b);
        }
    }

    fn add(&self) {
        let n = {
            let mut items = self.items.borrow_mut();
            let mut b = Bookmark {
                title: tr("New entry").to_owned(),
                ..Bookmark::default()
            };
            b.set_path(std::path::Path::new("/"));
            items.push(b);
            items.len()
        };
        self.rebuild(Some(n - 1));
        self.title.grab_focus();
    }

    fn remove(&self) {
        let Some(i) = self.selected() else { return };
        self.items.borrow_mut().remove(i);
        self.rebuild(Some(i));
    }

    fn move_by(&self, delta: isize) {
        let Some(i) = self.selected() else { return };
        let j = i as isize + delta;
        let n = self.items.borrow().len() as isize;
        if j < 0 || j >= n {
            return;
        }
        self.items.borrow_mut().swap(i, j as usize);
        self.rebuild(Some(j as usize));
    }
}

/// Open the hotlist editor: add, remove, rename, retarget and reorder entries. Enter saves,
/// Esc discards.
pub fn edit(ctx: &Rc<AppContext>) {
    let store = HistoryStore::ensure(ctx);
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Edit hotlist"));
    dialog.set_content_width(560);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Browse);
    list.add_css_class("boxed-list");
    list.update_property(&[gtk::accessible::Property::Label(tr("Hotlist entries"))]);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_min_content_height(200);
    scroller.set_max_content_height(360);
    scroller.set_propagate_natural_height(true);
    scroller.set_child(Some(&list));

    let title = gtk::Entry::new();
    title.update_property(&[gtk::accessible::Property::Label(tr("Title"))]);
    let path = gtk::Entry::new();
    path.update_property(&[gtk::accessible::Property::Label(tr("Directory"))]);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(8);
    for (row, (text, entry)) in [(tr("_Title"), &title), (tr("_Directory"), &path)]
        .into_iter()
        .enumerate()
    {
        let label = gtk::Label::with_mnemonic(text);
        label.set_xalign(0.0);
        label.set_mnemonic_widget(Some(entry));
        entry.set_hexpand(true);
        grid.attach(&label, 0, row as i32, 1, 1);
        grid.attach(entry, 1, row as i32, 1, 1);
    }

    let editor = Rc::new(Editor {
        items: RefCell::new(store.snapshot().hotlist),
        list: list.clone(),
        title: title.clone(),
        path: path.clone(),
        loading: Cell::new(false),
    });
    {
        let e = Rc::clone(&editor);
        list.connect_row_selected(move |_, _| e.load_selected());
        let e = Rc::clone(&editor);
        title.connect_changed(move |_| e.edited());
        let e = Rc::clone(&editor);
        path.connect_changed(move |_| e.edited());
    }

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let add_button = |label: &'static str, f: fn(&Editor)| {
        let b = gtk::Button::with_mnemonic(tr(label));
        let e = Rc::clone(&editor);
        b.connect_clicked(move |_| f(&e));
        buttons.append(&b);
    };
    add_button("_Add", Editor::add);
    add_button("_Remove", Editor::remove);
    add_button("Move _up", |e| e.move_by(-1));
    add_button("Move do_wn", |e| e.move_by(1));

    let column = modal::padded_column(10);
    column.append(&scroller);
    column.append(&grid);
    column.append(&buttons);
    dialog.set_child(Some(&modal::with_header(&column)));
    editor.rebuild(Some(0));
    dialog.set_focus(Some(&list));

    let (editor2, store2, ctx_ok) = (Rc::clone(&editor), Rc::clone(&store), Rc::clone(ctx));
    modal::present(ctx, &dialog, move || {
        if ui::activate_focused_button(&ctx_ok) {
            return false; // Add / Remove / Move: editing is not finished yet.
        }
        let kept: Vec<Bookmark> = editor2
            .items
            .borrow()
            .iter()
            .filter(|b| b.path_buf().is_some_and(|p| !p.as_os_str().is_empty()))
            .cloned()
            .collect();
        store2.update(|h| h.hotlist = kept);
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(default_title(std::path::Path::new("/a/b")), "b");
        assert_eq!(default_title(std::path::Path::new("/")), "/");
    }
}
