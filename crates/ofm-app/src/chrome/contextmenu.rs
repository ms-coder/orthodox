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
#[derive(Debug, Clone, Copy)]
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
        let Some(menu) = ctx.ext.get::<MenuSlot>().and_then(|s| s.current()) else {
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
/// pointer instead.
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
    let Some(rect) = rect else {
        return; // empty panel: nothing to act on
    };
    open(ctx, list, Some(rect));
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

/// Build and show the menu over `list`, pointing at `rect` (centre of `list` when `None`).
fn open(ctx: &Rc<AppContext>, list: &FileList, rect: Option<gdk::Rectangle>) {
    let Some(slot) = ctx.ext.get::<MenuSlot>() else {
        return;
    };
    close(ctx, &slot);

    let popover = gtk::Popover::new();
    popover.add_css_class("ofm-menu");
    popover.set_has_arrow(false);
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(gtk::Align::Start);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut rows = Vec::new();
    let mut pending_sep = false;
    for entry in ENTRIES {
        match entry {
            Sep => pending_sep = true,
            Cmd(cmd) => {
                if !ctx.command_available(*cmd) {
                    continue; // no handler: the entry is simply not shown
                }
                if pending_sep && !rows.is_empty() {
                    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
                }
                pending_sep = false;
                let row = build_row(ctx, &popover, *cmd);
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
    if let Some(r) = rect {
        popover.set_pointing_to(Some(&r));
    }

    let menu = Rc::new(ContextMenu { popover, rows });
    {
        let weak_ctx = Rc::downgrade(ctx);
        popover.connect_closed(move |p| {
            if let Some(ctx) = weak_ctx.upgrade()
                && let Some(slot) = ctx.ext.get::<MenuSlot>()
            {
                let is_current = slot.open.borrow().as_ref().is_some_and(|m| m.popover == *p);
                if is_current {
                    slot.open.borrow_mut().take();
                    p.unparent();
                }
            }
        });
    }
    *slot.open.borrow_mut() = Some(Rc::clone(&menu));
    // Fresh key hints, focus on the first row, then show.
    menu.refresh_accels(ctx);
    let first = menu.rows.first().map(|r| r.button.clone());
    popover.popup();
    if let Some(b) = first {
        glib::idle_add_local_once(move || {
            b.grab_focus();
        });
    }
}

/// Close the open menu (if any).
pub fn close(_ctx: &Rc<AppContext>, slot: &Rc<MenuSlot>) {
    if let Some(menu) = slot.open.borrow_mut().take() {
        menu.popover.popdown();
        menu.popover.unparent();
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
}
