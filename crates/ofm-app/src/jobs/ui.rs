//! Small widget helpers shared by the file-operation dialogs.

use std::rc::Rc;

use adw::prelude::*;

use crate::context::AppContext;
use crate::i18n::tr;
use crate::navigation::modal;

/// Hidden-until-needed label for validation errors (themed through `ofm-error`).
pub fn error_label() -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.add_css_class("ofm-error");
    l.set_visible(false);
    l.set_accessible_role(gtk::AccessibleRole::Alert);
    l
}

/// Show `msg` in `label` (or hide it for `None`).
pub fn set_error(label: &gtk::Label, msg: Option<&str>) {
    match msg {
        Some(m) => {
            label.set_text(m);
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

/// Left-aligned dimmed caption.
pub fn caption(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build()
}

/// Left-aligned plain label.
pub fn label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .build()
}

/// Push button with a mnemonic (`_Copy`) and an accessible name without the underscore.
pub fn button(mnemonic_label: &str) -> gtk::Button {
    let b = gtk::Button::with_mnemonic(mnemonic_label);
    b.update_property(&[gtk::accessible::Property::Label(
        &mnemonic_label.replace('_', ""),
    )]);
    b
}

/// Right-aligned row of buttons.
pub fn button_row(buttons: &[&gtk::Button]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    row.set_margin_top(6);
    for b in buttons {
        row.append(*b);
    }
    row
}

/// Enter pressed (`dialog.ok`) while a push button has the focus: press that button instead of
/// the dialog's default action. Returns true if a button was pressed.
pub fn activate_focused_button(ctx: &AppContext) -> bool {
    let Some(win) = ctx.window() else {
        return false;
    };
    let Some(focus) = gtk::prelude::RootExt::focus(&win.window) else {
        return false;
    };
    // `emit_clicked` (not `activate`): a mapped button delays `activate` by a visual 250 ms.
    if let Some(button) = focus.downcast_ref::<gtk::Button>() {
        button.emit_clicked();
        return true;
    }
    false
}

/// Show a plain message dialog with one OK button (Enter or Esc close it).
pub fn message(ctx: &Rc<AppContext>, title: &str, body: &str) {
    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(460);
    let column = modal::padded_column(10);
    let text = label(body);
    text.set_selectable(true);
    text.set_max_width_chars(70);
    column.append(&text);
    let ok = button(tr("_OK"));
    ok.add_css_class("suggested-action");
    column.append(&button_row(&[&ok]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&ok));
    let d = dialog.clone();
    ok.connect_clicked(move |_| {
        d.close();
    });
    modal::present(ctx, &dialog, || true);
}

/// Home directory for `~` expansion in path fields.
pub fn home() -> std::path::PathBuf {
    gtk::glib::home_dir()
}
