//! Small dialogs of the user menu run (F28): the `%{input:Prompt}` question and the
//! "run this command?" confirmation. Both report to a callback that runs after the dialog has
//! closed (so the next dialog of a chain is never stacked on a closing one); Esc cancels.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;

type Done<T> = Rc<RefCell<Option<Box<dyn FnOnce(T)>>>>;

/// Call the stored callback once with `value`, from an idle handler: the dialog that triggered
/// this is still closing, and the next dialog of a chain must not stack on top of it.
fn finish<T: 'static>(done: &Done<T>, value: T) {
    let cb = done.borrow_mut().take();
    if let Some(cb) = cb {
        gtk::glib::idle_add_local_once(move || cb(value));
    }
}

/// Ask for one line of text. `on_done(Some(text))` on OK/Enter, `on_done(None)` on Esc/Cancel.
pub fn ask(
    ctx: &Rc<AppContext>,
    title: &str,
    prompt: &str,
    on_done: impl FnOnce(Option<String>) + 'static,
) {
    let done: Done<Option<String>> = Rc::new(RefCell::new(Some(Box::new(on_done))));
    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(440);
    let column = modal::padded_column(8);
    column.append(&ui::label(prompt));
    let entry = gtk::Entry::new();
    entry.set_widget_name("ofm-usermenu-input");
    entry.update_property(&[gtk::accessible::Property::Label(prompt)]);
    column.append(&entry);
    let cancel = ui::button(tr("Cancel"));
    let ok = ui::button(tr("_OK"));
    ok.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &ok]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));

    let answer: Rc<RefCell<Option<String>>> = Rc::default();
    {
        let (answer, entry, weak) = (Rc::clone(&answer), entry.clone(), dialog.downgrade());
        ok.connect_clicked(move |_| {
            *answer.borrow_mut() = Some(entry.text().to_string());
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let weak = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let (done, answer) = (Rc::clone(&done), Rc::clone(&answer));
        dialog.connect_closed(move |_| {
            let a = answer.borrow_mut().take();
            finish(&done, a);
        });
    }
    let ctx_ok = Rc::clone(ctx);
    let shown = modal::present(ctx, &dialog, move || {
        if ui::activate_focused_button(&ctx_ok) {
            return false;
        }
        *answer.borrow_mut() = Some(entry.text().to_string());
        true
    });
    if !shown {
        finish(&done, None);
    }
}

/// Show the final `command` text and ask whether to run it. `on_done(true)` on Run/Enter.
pub fn confirm(
    ctx: &Rc<AppContext>,
    title: &str,
    command: &str,
    on_done: impl FnOnce(bool) + 'static,
) {
    let done: Done<bool> = Rc::new(RefCell::new(Some(Box::new(on_done))));
    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(520);
    let column = modal::padded_column(8);
    column.append(&ui::label(tr("This command will run:")));
    let text = gtk::Label::builder()
        .label(command)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .max_width_chars(70)
        .css_classes(["monospace"])
        .build();
    text.set_widget_name("ofm-usermenu-command");
    text.update_property(&[gtk::accessible::Property::Label(tr("Command"))]);
    column.append(&text);
    let cancel = ui::button(tr("Cancel"));
    let run = ui::button(tr("_Run"));
    run.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &run]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&run));

    let yes = Rc::new(std::cell::Cell::new(false));
    {
        let (yes, weak) = (Rc::clone(&yes), dialog.downgrade());
        run.connect_clicked(move |_| {
            yes.set(true);
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let weak = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let (done, yes) = (Rc::clone(&done), Rc::clone(&yes));
        dialog.connect_closed(move |_| finish(&done, yes.get()));
    }
    let ctx_ok = Rc::clone(ctx);
    let shown = modal::present(ctx, &dialog, move || {
        // Enter on the focused button presses it; nothing focused = Run.
        if ui::activate_focused_button(&ctx_ok) {
            return false;
        }
        yes.set(true);
        true
    });
    if !shown {
        finish(&done, false);
    }
}
