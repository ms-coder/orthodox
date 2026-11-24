//! The F9 menu bar (F32): drop-downs built from the command registry and the active keymap, so
//! every item is a `CommandId` run through `AppContext::execute` and shows its current binding.
//!
//! Keys while a drop-down is open: a window-level capture controller ([`follow_keys`]), added
//! after the dispatcher's so it runs first, handles Up/Down/Home/End (focus), Left/Right (switch
//! menu), Enter (run the focused item), Esc and F9 (close) and swallows everything else, so no
//! panel command fires behind the menu. Without it the dispatcher would resolve those keys in the
//! `panels` section. (Mouse use needs none of this.)

use std::rc::{Rc, Weak};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use ofm_core::{CommandId, KeyContext};

use super::menus::{Item, MENUS, MenuDef};
use crate::context::AppContext;
use crate::i18n::tr;

struct Row {
    cmd: CommandId,
    button: gtk::Button,
    accel: gtk::Label,
}

struct Menu {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    rows: Vec<Row>,
}

/// The menu bar widget.
pub struct MenuBar {
    /// Root widget (goes into `MainWindow::menubar_slot`).
    pub root: gtk::Box,
    menus: Vec<Menu>,
    ctx: Weak<AppContext>,
}

/// Key hint of `cmd` in the current keymap (empty when unbound): the first chord bound in the
/// panel/global sections.
pub fn accel_text(ctx: &AppContext, cmd: CommandId) -> String {
    ctx.keymap
        .borrow()
        .chords_for(cmd)
        .into_iter()
        .find(|(c, _)| matches!(c, KeyContext::Panels | KeyContext::Global))
        .map(|(_, chord)| chord.to_string())
        .unwrap_or_default()
}

impl MenuBar {
    /// Build the bar.
    pub fn new(ctx: &Rc<AppContext>) -> Rc<MenuBar> {
        Rc::new_cyclic(|weak: &Weak<MenuBar>| {
            let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            root.add_css_class("ofm-menubar");
            let menus: Vec<Menu> = MENUS
                .iter()
                .enumerate()
                .map(|(i, def)| build_menu(ctx, weak, i, def))
                .collect();
            for m in &menus {
                root.append(&m.button);
            }
            MenuBar {
                root,
                menus,
                ctx: Rc::downgrade(ctx),
            }
        })
    }

    /// Number of drop-downs.
    pub fn menu_count(&self) -> usize {
        self.menus.len()
    }

    /// Is any drop-down open?
    pub fn is_open(&self) -> bool {
        self.menus.iter().any(|m| m.popover.is_visible())
    }

    /// Open drop-down `i` (closing the others).
    pub fn open(&self, i: usize) {
        self.close();
        if let Some(m) = self.menus.get(i) {
            self.refresh_accels(i);
            m.button.popup();
        }
    }

    /// Close every drop-down.
    pub fn close(&self) {
        for m in &self.menus {
            if m.popover.is_visible() {
                m.popover.popdown();
            }
        }
    }

    /// `app.menu`: open the first drop-down, or close the open one.
    pub fn toggle(&self) {
        if self.is_open() {
            self.close();
        } else {
            self.open(0);
        }
    }

    /// Titles of the rows of menu `i` (tests).
    pub fn row_commands(&self, i: usize) -> Vec<CommandId> {
        self.menus
            .get(i)
            .map(|m| m.rows.iter().map(|r| r.cmd).collect())
            .unwrap_or_default()
    }

    /// Key hint shown for the row of `cmd` in menu `i` (tests).
    pub fn row_accel(&self, i: usize, cmd: CommandId) -> Option<String> {
        self.menus
            .get(i)?
            .rows
            .iter()
            .find(|r| r.cmd == cmd)
            .map(|r| r.accel.text().to_string())
    }

    /// Re-read the key hints of menu `i` from the current keymap.
    fn refresh_accels(&self, i: usize) {
        let (Some(menu), Some(ctx)) = (self.menus.get(i), self.ctx.upgrade()) else {
            return;
        };
        for r in &menu.rows {
            r.accel.set_text(&accel_text(&ctx, r.cmd));
        }
    }

    /// A drop-down became visible (also when opened by a mouse click): fresh hints, focus on the
    /// first row.
    fn on_shown(&self, i: usize) {
        self.refresh_accels(i);
        let first = self
            .menus
            .get(i)
            .and_then(|m| m.rows.first())
            .map(|r| r.button.clone());
        glib::idle_add_local_once(move || {
            if let Some(b) = first {
                b.grab_focus();
            }
        });
    }

    fn open_index(&self) -> Option<usize> {
        self.menus.iter().position(|m| m.popover.is_visible())
    }

    /// Move the focus by `delta` rows in the open menu (wraps; `to_end` jumps to the ends).
    fn move_focus(&self, i: usize, delta: isize, to_end: bool) {
        let Some(menu) = self.menus.get(i) else {
            return;
        };
        let n = menu.rows.len();
        if n == 0 {
            return;
        }
        let cur = menu.rows.iter().position(|r| r.button.has_focus());
        let target = match (cur, to_end) {
            (_, true) if delta < 0 => 0,
            (_, true) => n - 1,
            (Some(c), false) => (c + n).wrapping_add_signed(delta) % n,
            (None, false) if delta < 0 => n - 1,
            (None, false) => 0,
        };
        menu.rows[target].button.grab_focus();
    }

    /// Handle a key press while a drop-down is open. Returns true when the key was consumed (all
    /// keys are, except bare modifiers).
    pub fn handle_key(&self, key: gdk::Key) -> bool {
        let Some(i) = self.open_index() else {
            return false;
        };
        match key {
            gdk::Key::Escape | gdk::Key::F9 => self.close(),
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => self.activate_focused(i),
            gdk::Key::Up => self.move_focus(i, -1, false),
            gdk::Key::Down => self.move_focus(i, 1, false),
            gdk::Key::Home => self.move_focus(i, -1, true),
            gdk::Key::End => self.move_focus(i, 1, true),
            gdk::Key::Left => self.step(i, -1),
            gdk::Key::Right => self.step(i, 1),
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

    fn activate_focused(&self, i: usize) {
        if let Some(r) = self
            .menus
            .get(i)
            .and_then(|m| m.rows.iter().find(|r| r.button.has_focus()))
        {
            r.button.emit_clicked();
        }
    }

    /// Switch to the neighbouring menu (`delta` = -1 / +1), wrapping around.
    fn step(&self, i: usize, delta: isize) {
        let n = self.menus.len();
        if n == 0 {
            return;
        }
        let j = (i + n).wrapping_add_signed(delta) % n;
        self.open(j);
    }
}

fn build_menu(ctx: &Rc<AppContext>, bar: &Weak<MenuBar>, index: usize, def: &MenuDef) -> Menu {
    let title = tr(def.title);
    let popover = gtk::Popover::new();
    popover.add_css_class("ofm-menu");
    popover.set_has_arrow(false);
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(gtk::Align::Start);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut rows = Vec::new();
    for item in def.items {
        match item {
            Item::Sep => list.append(&gtk::Separator::new(gtk::Orientation::Horizontal)),
            Item::Cmd(cmd) => {
                let row = build_row(ctx, &popover, *cmd);
                list.append(&row.button);
                rows.push(row);
            }
        }
    }
    popover.set_child(Some(&list));

    let button = gtk::MenuButton::new();
    // A label child (not `set_label`) keeps the dropdown arrow away.
    button.set_child(Some(&gtk::Label::new(Some(title))));
    button.set_popover(Some(&popover));
    button.set_has_frame(false);
    button.set_focusable(false);
    button.set_focus_on_click(false);
    button.add_css_class("ofm-menu-title");
    button.update_property(&[gtk::accessible::Property::Label(title)]);

    {
        let bar = bar.clone();
        popover.connect_visible_notify(move |p| {
            if p.is_visible()
                && let Some(bar) = bar.upgrade()
            {
                bar.on_shown(index);
            }
        });
    }

    Menu {
        button,
        popover,
        rows,
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
    // A command without a handler (not implemented yet) is greyed out, not silently dead.
    button.set_sensitive(ctx.command_available(cmd));
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    {
        let popover = popover.clone();
        let weak = Rc::downgrade(ctx);
        button.connect_clicked(move |_| {
            popover.popdown();
            // Run after the drop-down is gone, so a dialog opened by the command starts from a
            // clean state.
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

/// Install the window-level key handler described in the module docs. Call it after the
/// dispatcher's controller exists (i.e. from `install_ui`).
pub fn follow_keys(window: &impl IsA<gtk::Window>, bar: &Rc<MenuBar>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(bar);
    keys.connect_key_pressed(move |_, key, _, _| match weak.upgrade() {
        Some(bar) if bar.handle_key(key) => glib::Propagation::Stop,
        _ => glib::Propagation::Proceed,
    });
    window.as_ref().add_controller(keys);
}
