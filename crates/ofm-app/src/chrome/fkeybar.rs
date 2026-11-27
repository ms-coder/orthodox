//! The F-key bar (F32): ten clickable cells showing the command bound to F1..F10 in the current
//! keymap. Holding Shift/Ctrl/Alt switches the labels to the modified bindings (`Shift+F5` ...).

use std::cell::Cell;
use std::rc::{Rc, Weak};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use ofm_core::{CommandId, Key, KeyChord, Mods};

use super::labels::{command_title, short_label};
use crate::context::AppContext;
use crate::i18n::tr;

const KEYS: u8 = 10;

struct FCell {
    button: gtk::Button,
    label: gtk::Label,
    /// Command currently shown (what a click runs).
    command: Cell<Option<CommandId>>,
}

/// The bar widget and its state.
pub struct FKeyBar {
    /// Root widget (goes into `MainWindow::fkeybar_slot`).
    pub root: gtk::Box,
    cells: Vec<Rc<FCell>>,
    mods: Cell<Mods>,
    ctx: Weak<AppContext>,
}

/// Command bound to `F<n>` with `mods` in the main window's keymap (no command-line text).
pub fn command_for(ctx: &AppContext, n: u8, mods: Mods) -> Option<CommandId> {
    let chord = KeyChord {
        key: Key::F(n),
        mods,
    };
    ctx.keymap.borrow().resolve_main(chord, false)
}

/// Modifier set from a GDK state (Shift, Ctrl, Alt only: Super is not part of F-key variants).
pub fn mods_from_state(state: gdk::ModifierType) -> Mods {
    Mods {
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        super_: false,
    }
}

impl FKeyBar {
    /// Build the bar for `ctx` (labels for no modifiers).
    pub fn new(ctx: &Rc<AppContext>) -> Rc<FKeyBar> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_homogeneous(true);
        root.add_css_class("ofm-fkeybar");
        let mut cells = Vec::new();
        for n in 1..=KEYS {
            let num = gtk::Label::new(Some(&n.to_string()));
            num.add_css_class("ofm-fkey-num");
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("ofm-fkey-label");
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            content.append(&num);
            content.append(&label);
            let button = gtk::Button::new();
            button.set_child(Some(&content));
            button.set_has_frame(false);
            button.set_focusable(false);
            button.set_focus_on_click(false);
            button.add_css_class("ofm-fkey");
            root.append(&button);
            cells.push(Rc::new(FCell {
                button,
                label,
                command: Cell::new(None),
            }));
        }
        let bar = Rc::new(FKeyBar {
            root,
            cells,
            mods: Cell::new(Mods::default()),
            ctx: Rc::downgrade(ctx),
        });
        for cell in &bar.cells {
            let weak = Rc::downgrade(&bar);
            let cell = Rc::clone(cell);
            let button = cell.button.clone();
            button.connect_clicked(move |_| {
                let Some(bar) = weak.upgrade() else { return };
                if let (Some(ctx), Some(cmd)) = (bar.ctx.upgrade(), cell.command.get()) {
                    ctx.execute(cmd);
                }
            });
        }
        bar.refresh();
        bar
    }

    /// Show the variants for `mods` (called when the held modifiers change).
    pub fn set_mods(&self, mods: Mods) {
        if self.mods.replace(mods) != mods {
            self.refresh();
        }
    }

    /// Currently shown modifier set.
    pub fn mods(&self) -> Mods {
        self.mods.get()
    }

    /// Re-read the keymap (after `app.reload_config`) and update every cell.
    pub fn refresh(&self) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let mods = self.mods.get();
        for (i, cell) in self.cells.iter().enumerate() {
            let n = u8::try_from(i + 1).unwrap_or(1);
            let cmd = command_for(&ctx, n, mods);
            cell.command.set(cmd);
            let text = cmd.map(short_label).unwrap_or_default();
            cell.label.set_text(&text);
            // A key without a command, or a command without a handler (not implemented yet),
            // shows an empty, insensitive cell instead of silently doing nothing.
            let available = cmd.is_some_and(|c| ctx.command_available(c));
            cell.button.set_sensitive(available);
            if cmd.is_some() {
                cell.button.remove_css_class("empty");
            } else {
                cell.button.add_css_class("empty");
            }
            let name = match cmd {
                Some(c) => format!("F{n}: {}", tr(command_title(&ctx, c))),
                None => format!("F{n}"),
            };
            cell.button
                .update_property(&[gtk::accessible::Property::Label(&name)]);
        }
    }

    /// Whether cell `n` (1..=10) takes clicks: it shows a command that has a handler (tests).
    pub fn is_enabled(&self, n: usize) -> bool {
        n.checked_sub(1)
            .and_then(|i| self.cells.get(i))
            .is_some_and(|c| c.button.is_sensitive())
    }

    /// Texts of the ten labels (tests).
    pub fn labels(&self) -> Vec<String> {
        self.cells
            .iter()
            .map(|c| c.label.text().to_string())
            .collect()
    }

    /// Run the command of cell `n` (1..=10) as a click would (tests).
    pub fn click(&self, n: usize) {
        if let Some(c) = n.checked_sub(1).and_then(|i| self.cells.get(i)) {
            c.button.emit_clicked();
        }
    }
}

/// Track the held modifiers on `window` (capture phase, never consumes keys) and reset them when
/// the window loses focus, so a Shift released elsewhere does not leave the bar in Shift mode.
pub fn follow_modifiers(window: &impl IsA<gtk::Window>, bar: &Rc<FKeyBar>) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(bar);
    controller.connect_modifiers(move |_, state| {
        if let Some(bar) = weak.upgrade() {
            bar.set_mods(mods_from_state(state));
        }
        glib::Propagation::Proceed
    });
    window.as_ref().add_controller(controller);
    let weak = Rc::downgrade(bar);
    window.as_ref().connect_is_active_notify(move |w| {
        if !w.is_active()
            && let Some(bar) = weak.upgrade()
        {
            bar.set_mods(Mods::default());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_mapping() {
        let m = mods_from_state(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK);
        assert!(m.shift && m.alt && !m.ctrl && !m.super_);
        assert_eq!(mods_from_state(gdk::ModifierType::empty()), Mods::default());
        // Caps lock / num lock never count.
        assert_eq!(
            mods_from_state(gdk::ModifierType::LOCK_MASK),
            Mods::default()
        );
    }
}
