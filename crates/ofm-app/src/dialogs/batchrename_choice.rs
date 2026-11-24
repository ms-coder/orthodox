//! A small "pick one button" dialog used by the batch rename flow (T8.3c): waiting for the
//! editor, problems with the edited text, the preview, the rollback offer.
//!
//! Keys: Enter presses the focused button, or the default one when focus is elsewhere; Esc (and
//! closing the window) answers with the cancel button's index. The answer is delivered once,
//! after the dialog has closed (so a follow-up dialog never stacks on a closing one).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::context::AppContext;
use crate::jobs::ui;
use crate::navigation::modal;

/// One button of the dialog.
pub struct ButtonSpec {
    /// Label with a mnemonic (`_Apply`).
    pub label: &'static str,
    /// Widget name (tests find the button by it).
    pub name: &'static str,
    /// Gets the `suggested-action` style.
    pub suggested: bool,
}

/// Content and behaviour of a [`present`]ed dialog.
pub struct ChoiceSpec {
    /// Window title.
    pub title: &'static str,
    /// Content width in pixels.
    pub width: i32,
    /// Widgets above the button row.
    pub content: Vec<gtk::Widget>,
    /// Buttons, left to right.
    pub buttons: Vec<ButtonSpec>,
    /// Index of the button Enter acts on (and the initial focus).
    pub default: usize,
    /// Index answered for Esc and for closing the window.
    pub cancel: usize,
}

type AnswerFn = Box<dyn FnOnce(usize)>;

/// Show the dialog; `on_choice` gets the index of the chosen button after the dialog closed.
/// When no dialog can be shown (no main window) `on_choice(cancel)` is called at once.
pub fn present(ctx: &Rc<AppContext>, spec: ChoiceSpec, on_choice: impl FnOnce(usize) + 'static) {
    let ChoiceSpec {
        title,
        width,
        content,
        buttons,
        default,
        cancel,
    } = spec;
    let done: Rc<RefCell<Option<AnswerFn>>> = Rc::new(RefCell::new(Some(Box::new(on_choice))));
    let chosen = Rc::new(Cell::new(cancel));

    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(width);
    let column = modal::padded_column(8);
    for w in &content {
        column.append(w);
    }
    let widgets: Vec<gtk::Button> = buttons
        .iter()
        .map(|b| {
            let button = ui::button(b.label);
            button.set_widget_name(b.name);
            if b.suggested {
                button.add_css_class("suggested-action");
            }
            button
        })
        .collect();
    let refs: Vec<&gtk::Button> = widgets.iter().collect();
    column.append(&ui::button_row(&refs));
    dialog.set_child(Some(&modal::with_header(&column)));
    if let Some(b) = widgets.get(default) {
        dialog.set_focus(Some(b));
    }
    for (i, button) in widgets.iter().enumerate() {
        let (chosen, weak) = (Rc::clone(&chosen), dialog.downgrade());
        button.connect_clicked(move |_| {
            chosen.set(i);
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let (done, chosen) = (Rc::clone(&done), Rc::clone(&chosen));
        dialog.connect_closed(move |_| {
            let cb = done.borrow_mut().take();
            if let Some(cb) = cb {
                let i = chosen.get();
                gtk::glib::idle_add_local_once(move || cb(i));
            }
        });
    }
    let (ctx_ok, chosen_ok) = (Rc::clone(ctx), Rc::clone(&chosen));
    let shown = modal::present(ctx, &dialog, move || {
        // Enter on a focused button presses that button (it closes the dialog itself).
        if ui::activate_focused_button(&ctx_ok) {
            return false;
        }
        chosen_ok.set(default);
        true
    });
    if !shown {
        let cb = done.borrow_mut().take();
        if let Some(cb) = cb {
            cb(cancel);
        }
    }
}
