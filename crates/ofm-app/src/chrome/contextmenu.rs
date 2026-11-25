//! The panel context menu (`panel.context_menu`): right click, Shift+F10 and the Menu key open
//! a popover at the pointer (at the cursor row when opened from the keyboard) with the file
//! commands. Entries are registry commands shown only when a handler is registered, and every
//! entry runs through `AppContext::execute` (invariant 5). A right click on an unselected row
//! has already moved the cursor there (the FileList does that before emitting `context-menu`).
//!
//! Keys while the menu is open work like in the F9 menu bar: a window-level capture controller
//! ([`install_ui`], added after the dispatcher's, so it runs first) handles Up/Down/Home/End
//! (focus), Enter (run the focused entry), Esc (close) and swallows everything else, so no
//! panel command fires behind the menu.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use ofm_core::{CommandId, cmd};

use super::menubar::accel_text;
use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::widgets::file_list::FileList;
use crate::window::MainWindow;

/// One row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// A registry command.
    Cmd(CommandId),
    /// Separator line.
    Sep,
}

use Entry::{Cmd, Sep};

/// Rows in menu order. `Cmd` entries whose command has no registered handler are not shown.
const ENTRIES: &[Entry] = &[
    Cmd(cmd::PANEL_OPEN),
    Cmd(cmd::FILE_VIEW),
    Cmd(cmd::FILE_EDIT),
    Sep,
    Cmd(cmd::FILE_COPY),
    Cmd(cmd::FILE_MOVE),
    Cmd(cmd::FILE_RENAME),
    Cmd(cmd::FILE_DELETE),
    Sep,
    Cmd(cmd::FILE_PROPERTIES),
    Cmd(cmd::FILE_COPY_PATH),
    Sep,
    Cmd(cmd::SELECT_BY_MASK),
    Cmd(cmd::PANEL_COMPARE_DIRS),
    Cmd(cmd::FILE_BATCH_RENAME),
];

struct Row {
    cmd: CommandId,
    button: gtk::Button,
    accel: gtk::Label,
}

/// The open menu (state in `ctx.ext`, [`MenuSlot`]); only one is ever open.
pub struct ContextMenu {
    popover: gtk::Popover,
    rows: Vec<Row>,
}

/// Slot for the open menu and the last secondary click in `ctx.ext`.
#[derive(Default)]
pub struct MenuSlot {
    open: RefCell<Option<Rc<ContextMenu>>>,
    /// Where the current command was triggered from (set by [`open_from_click`], taken by the
    /// handler): the list and the pointer position in its widget coordinates.
    click: RefCell<Option<(FileList, f64, f64)>>,
}

impl MenuSlot {
    fn current(&self) -> Option<Rc<ContextMenu>> {
        self.open.borrow().clone()
    }
}

/// The menu that is open now, if any.
pub fn current(ctx: &AppContext) -> Option<Rc<ContextMenu>> {
    ctx.ext
        .get::<MenuSlot>()
        .and_then(|s| s.current())
        .filter(|m| m.is_open())
}

/// Is a context menu open?
pub fn is_open(ctx: &AppContext) -> bool {
    current(ctx).is_some()
}

/// The rows to build when `available` tells whether a command has a registered handler (pure):
/// entries without one are not shown, and separators that would then lead, trail or repeat go
/// with them.
fn shown_entries(available: impl Fn(CommandId) -> bool) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for entry in ENTRIES {
        match entry {
            Sep => {
                if !out.is_empty() && out.last() != Some(&Sep) {
                    out.push(Sep);
                }
            }
            Cmd(cmd) => {
                if available(*cmd) {
                    out.push(*entry);
                }
            }
        }
    }
    if out.last() == Some(&Sep) {
        out.pop();
    }
    out
}

/// Register `panel.context_menu` (keyboard: open at the cursor row).
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_CONTEXT_MENU, open_at_cursor);
}

/// Create the slot.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(MenuSlot::default()));
}

/// Install the window-level key controller described in the module docs (call after the
/// dispatcher's controller exists, i.e. from `chrome::install_ui`).
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ctx);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(ctx) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let Some(menu) = current(&ctx) else {
            return glib::Propagation::Proceed;
        };
        match menu.handle_key(key) {
            true => glib::Propagation::Stop,
            false => glib::Propagation::Proceed,
        }
    });
    win.window.add_controller(keys);
}

/// `panel.context_menu` (Shift+F10, Menu key): open at the active panel's cursor row. When the
/// command was triggered by a secondary click ([`open_from_click`]) the menu points at the
/// pointer instead. (The click activated the clicked panel first, so it is the active one.)
fn open_at_cursor(ctx: &Rc<AppContext>) {
    let Some(win) = ctx.window() else {
        return;
    };
    let side = ctx.active_side();
    let list = if side == 0 {
        &win.left.list
    } else {
        &win.right.list
    };
    // A remembered click on exactly this list opens the menu at the pointer.
    let rect = ctx
        .ext
        .get::<MenuSlot>()
        .and_then(|slot| slot.click.borrow_mut().take())
        .filter(|(clicked, _, _)| *clicked == *list)
        .map(|(_, x, y)| gdk::Rectangle::new(x as i32, y as i32, 1, 1))
        .or_else(|| {
            list.cursor_row_rect().map(|r| {
                gdk::Rectangle::new(
                    r.x as i32,
                    r.y as i32,
                    r.width.max(1.0) as i32,
                    r.height.max(1.0) as i32,
                )
            })
        });
    // An empty panel has no cursor row: the menu opens over the middle of the list.
    let rect =
        rect.unwrap_or_else(|| gdk::Rectangle::new(list.width() / 2, list.height() / 2, 1, 1));
    open(ctx, list, rect);
}

/// Secondary click at (x, y) of `list` (widget coordinates; the FileList emits the signal):
/// remember the position, then dispatch `panel.context_menu` itself (invariant 5), so the menu
/// opens at the pointer for the mouse and at the cursor row for the keyboard.
pub fn open_from_click(ctx: &Rc<AppContext>, list: &FileList, x: f64, y: f64) {
    if let Some(slot) = ctx.ext.get::<MenuSlot>() {
        *slot.click.borrow_mut() = Some((list.clone(), x, y));
    }
    ctx.execute(cmd::PANEL_CONTEXT_MENU);
}

/// Build and show the menu over `list`, pointing at `rect`.
fn open(ctx: &Rc<AppContext>, list: &FileList, rect: gdk::Rectangle) {
    let Some(slot) = ctx.ext.get::<MenuSlot>() else {
        return;
    };
    dismiss(&slot);

    let popover = gtk::Popover::new();
    popover.add_css_class("ofm-menu");
    popover.set_has_arrow(false);
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(gtk::Align::Start);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut rows = Vec::new();
    // Entries whose command is unregistered are simply not shown.
    for entry in shown_entries(|c| ctx.command_available(c)) {
        match entry {
            Sep => column.append(&gtk::Separator::new(gtk::Orientation::Horizontal)),
            Cmd(cmd) => {
                let row = build_row(ctx, &popover, cmd);
                column.append(&row.button);
                rows.push(row);
            }
        }
    }
    if rows.is_empty() {
        return; // nothing runnable
    }
    popover.set_child(Some(&column));
    popover.set_parent(list);
    popover.set_pointing_to(Some(&rect));

    let menu = Rc::new(ContextMenu { popover, rows });
    {
        let weak_ctx = Rc::downgrade(ctx);
        let weak_menu = Rc::downgrade(&menu);
        // Closed by any means (Esc, click outside, a command): forget it and unparent it.
        menu.popover.connect_closed(move |_| {
            if let (Some(ctx), Some(menu)) = (weak_ctx.upgrade(), weak_menu.upgrade())
                && let Some(slot) = ctx.ext.get::<MenuSlot>()
                && slot.current().is_some_and(|m| Rc::ptr_eq(&m, &menu))
            {
                *slot.open.borrow_mut() = None;
                menu.popover.unparent();
            }
        });
    }
    *slot.open.borrow_mut() = Some(Rc::clone(&menu));
    // Fresh key hints, focus on the first row, then show.
    menu.refresh_accels(ctx);
    let first = menu.rows.first().map(|r| r.button.clone());
    menu.popover.popup();
    if let Some(b) = first {
        glib::idle_add_local_once(move || {
            b.grab_focus();
        });
    }
}

/// Close the open menu (if any). Closing runs the popover's `closed` handler, which forgets
/// the menu and unparents the popover.
fn dismiss(slot: &Rc<MenuSlot>) {
    if let Some(menu) = slot.open.borrow().clone() {
        menu.popover.popdown();
        *slot.open.borrow_mut() = None;
        menu.popover.unparent();
    }
}

/// Close the open context menu (if any).
pub fn close(ctx: &AppContext) {
    if let Some(slot) = ctx.ext.get::<MenuSlot>() {
        dismiss(&slot);
    }
}

fn build_row(ctx: &Rc<AppContext>, popover: &gtk::Popover, cmd: CommandId) -> Row {
    let title = cmd.info().map_or(cmd.as_str(), |i| tr(i.title));
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    let accel = gtk::Label::new(None);
    accel.set_xalign(1.0);
    accel.add_css_class("ofm-menu-accel");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&label);
    content.append(&accel);
    let button = gtk::Button::new();
    button.set_child(Some(&content));
    button.set_has_frame(false);
    button.add_css_class("ofm-menu-item");
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    {
        let popover = popover.clone();
        let weak = Rc::downgrade(ctx);
        button.connect_clicked(move |_| {
            popover.popdown();
            // Run after the menu is gone, so a dialog opened by the command starts clean.
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(ctx) = weak.upgrade() {
                    ctx.execute(cmd);
                }
            });
        });
    }
    Row { cmd, button, accel }
}

impl ContextMenu {
    /// Re-read the key hints from the current keymap.
    fn refresh_accels(&self, ctx: &Rc<AppContext>) {
        for r in &self.rows {
            r.accel.set_text(&accel_text(ctx, r.cmd));
        }
    }

    /// Handle a key press while the menu is open: returns true when the key was consumed (all
    /// keys are, except bare modifiers). Mirrors `MenuBar::handle_key`.
    fn handle_key(&self, key: gdk::Key) -> bool {
        match key {
            gdk::Key::Escape => self.popover.popdown(),
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => self.activate_focused(),
            gdk::Key::Up => self.move_focus(-1, false),
            gdk::Key::Down => self.move_focus(1, false),
            gdk::Key::Home => self.move_focus(-1, true),
            gdk::Key::End => self.move_focus(1, true),
            gdk::Key::Shift_L
            | gdk::Key::Shift_R
            | gdk::Key::Control_L
            | gdk::Key::Control_R
            | gdk::Key::Alt_L
            | gdk::Key::Alt_R
            | gdk::Key::Super_L
            | gdk::Key::Super_R => return false,
            _ => {}
        }
        true
    }

    fn activate_focused(&self) {
        if let Some(r) = self.rows.iter().find(|r| r.button.has_focus()) {
            r.button.emit_clicked();
        }
    }

    /// Move the focus by `delta` rows (wraps; `to_end` jumps to the ends).
    fn move_focus(&self, delta: isize, to_end: bool) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let cur = self.rows.iter().position(|r| r.button.has_focus());
        let target = match (cur, to_end) {
            (_, true) if delta < 0 => 0,
            (_, true) => n - 1,
            (Some(c), false) => (c + n).wrapping_add_signed(delta) % n,
            (None, false) if delta < 0 => n - 1,
            (None, false) => 0,
        };
        self.rows[target].button.grab_focus();
    }

    /// Run the row of `cmd` like a click on it (the command runs after the menu is gone);
    /// false when `cmd` has no row.
    pub fn activate(&self, cmd: CommandId) -> bool {
        match self.rows.iter().find(|r| r.cmd == cmd) {
            Some(r) => {
                r.button.emit_clicked();
                true
            }
            None => false,
        }
    }

    /// Command ids of the shown rows (tests).
    pub fn shown_commands(&self) -> Vec<CommandId> {
        self.rows.iter().map(|r| r.cmd).collect()
    }

    /// Key hint shown for `cmd` (tests).
    pub fn accel_of(&self, cmd: CommandId) -> Option<String> {
        self.rows
            .iter()
            .find(|r| r.cmd == cmd)
            .map(|r| r.accel.text().to_string())
    }

    /// Whether the menu popover is visible (tests).
    pub fn is_open(&self) -> bool {
        self.popover.is_visible()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_known_commands_without_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for e in ENTRIES {
            if let Cmd(id) = e {
                assert!(id.info().is_some(), "{id} is not in the command registry");
                assert!(seen.insert(*id), "{id} listed twice");
            }
        }
        // No leading/trailing/double separators in the definition.
        assert!(matches!(ENTRIES.first(), Some(Cmd(_))));
        assert!(matches!(ENTRIES.last(), Some(Cmd(_))));
        for pair in ENTRIES.windows(2) {
            assert!(!matches!(pair, [Sep, Sep]));
        }
    }

    #[test]
    fn unregistered_commands_are_not_shown() {
        // Everything registered: the definition as is (separators included).
        assert_eq!(shown_entries(|_| true), ENTRIES.to_vec());
        // Nothing registered: no rows at all, not even a lone separator.
        assert!(shown_entries(|_| false).is_empty());
        // The first group and the last three entries are unregistered: their separators go too.
        let out = shown_entries(|c| {
            !matches!(
                c,
                cmd::PANEL_OPEN
                    | cmd::FILE_VIEW
                    | cmd::FILE_EDIT
                    | cmd::SELECT_BY_MASK
                    | cmd::PANEL_COMPARE_DIRS
                    | cmd::FILE_BATCH_RENAME
            )
        });
        assert_eq!(
            out,
            vec![
                Cmd(cmd::FILE_COPY),
                Cmd(cmd::FILE_MOVE),
                Cmd(cmd::FILE_RENAME),
                Cmd(cmd::FILE_DELETE),
                Sep,
                Cmd(cmd::FILE_PROPERTIES),
                Cmd(cmd::FILE_COPY_PATH),
            ]
        );
        // A hole in the middle leaves one separator between each pair of surviving groups.
        let out = shown_entries(|c| {
            !matches!(
                c,
                cmd::FILE_COPY | cmd::FILE_MOVE | cmd::FILE_RENAME | cmd::FILE_DELETE
            )
        });
        assert_eq!(out.iter().filter(|e| **e == Sep).count(), 2, "{out:?}");
    }
}
