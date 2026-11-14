//! The F9 menu bar (F32): drop-downs built from the command registry and the active keymap, so
//! every item is a `CommandId` run through `AppContext::execute` and shows its current binding.
//!
//! While a drop-down is open the app is in modal mode (keys resolve in the `dialog` section) and
//! Enter / Esc arrive as `dialog.ok` / `dialog.cancel`, routed here through `window::modal_keys`.
//! Up/Down move the focus natively; Left/Right switch to the neighbouring menu.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use ofm_core::{CommandId, KeyContext};

use super::menus::{Item, MENUS, MenuDef};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::window::modal_keys::{self, ModalHandler, ModalKeysGuard};

struct Row {
    cmd: CommandId,
    button: gtk::Button,
    accel: gtk::Label,
}

struct Menu {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    rows: Vec<Row>,
    guard: RefCell<Option<ModalKeysGuard>>,
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

    fn on_show(self: &Rc<Self>, i: usize) {
        let (Some(menu), Some(ctx)) = (self.menus.get(i), self.ctx.upgrade()) else {
            return;
        };
        for r in &menu.rows {
            r.accel.set_text(&accel_text(&ctx, r.cmd));
            eprintln!("DBG {} -> {:?}", r.cmd, r.accel.text());
        }
        let ok = Rc::downgrade(self);
        let cancel = Rc::downgrade(self);
        let guard = modal_keys::push(
            &ctx,
            ModalHandler {
                ok: Box::new(move || {
                    if let Some(bar) = ok.upgrade() {
                        bar.activate_focused(i);
                    }
                }),
                cancel: Box::new(move || {
                    if let Some(bar) = cancel.upgrade() {
                        bar.close();
                    }
                }),
            },
        );
        *menu.guard.borrow_mut() = Some(guard);
        let first = menu.rows.first().map(|r| r.button.clone());
        glib::idle_add_local_once(move || {
            if let Some(b) = first {
                b.grab_focus();
            }
        });
    }

    fn on_closed(&self, i: usize) {
        if let Some(menu) = self.menus.get(i) {
            menu.guard.borrow_mut().take();
        }
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
        popover.connect_map(move |_| {
            if let Some(bar) = bar.upgrade() {
                bar.on_show(index);
            }
        });
    }
    {
        let bar = bar.clone();
        popover.connect_closed(move |_| {
            if let Some(bar) = bar.upgrade() {
                bar.on_closed(index);
            }
        });
    }
    // Left/Right switch menus (captured before the popover's own focus movement).
    {
        let bar = bar.clone();
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, _| {
            let delta = match key {
                gdk::Key::Left => -1,
                gdk::Key::Right => 1,
                _ => return glib::Propagation::Proceed,
            };
            if let Some(bar) = bar.upgrade() {
                bar.step(index, delta);
            }
            glib::Propagation::Stop
        });
        popover.add_controller(keys);
    }

    Menu {
        button,
        popover,
        rows,
        guard: RefCell::new(None),
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
