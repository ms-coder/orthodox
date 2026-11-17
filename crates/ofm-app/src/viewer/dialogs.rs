//! The viewer's three small dialogs: search (F7), go to line/offset (Alt+F8) and encoding (F8).
//! They are `adw::Dialog`s presented over the viewer window through `modal::present_over`, so
//! the keymap's `dialog` section drives them (Enter = OK, Esc = cancel, Tab / Space natively).

use std::rc::Rc;

use adw::prelude::*;
use ofm_viewer::{Encoding, Needle, SearchOptions};

use super::session::Session;
use super::worker::{Dir, SearchSpec, parse_goto, parse_hex};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;

/// Hex digits of `bytes` for the dialog entry (`48 65 6C`).
fn hex_text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn radio(label: &str, group: Option<&gtk::CheckButton>) -> gtk::CheckButton {
    let b = gtk::CheckButton::with_mnemonic(label);
    if let Some(g) = group {
        b.set_group(Some(g));
    }
    b.update_property(&[gtk::accessible::Property::Label(&label.replace('_', ""))]);
    b
}

/// Wire a pair of Cancel/OK buttons and the Enter handler; returns nothing.
fn finish(
    ctx: &Rc<AppContext>,
    session: &Rc<Session>,
    dialog: &adw::Dialog,
    cancel: &gtk::Button,
    ok: &gtk::Button,
    submit: Rc<dyn Fn() -> bool>,
) {
    let weak = dialog.downgrade();
    {
        let (submit, weak) = (Rc::clone(&submit), weak.clone());
        ok.connect_clicked(move |_| {
            if submit()
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
        });
    }
    cancel.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    let dialog_weak = dialog.downgrade();
    modal::present_over(ctx, dialog, &session.window, move || {
        // Enter on a focused button presses that button instead.
        if let Some(d) = dialog_weak.upgrade()
            && let Some(focus) = d.focus()
            && let Some(button) = focus.downcast_ref::<gtk::Button>()
        {
            button.emit_clicked();
            return false;
        }
        submit()
    });
}

/// F7: search dialog.
pub fn search(ctx: &Rc<AppContext>, session: &Rc<Session>) {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Search"));
    dialog.set_content_width(460);
    let column = modal::padded_column(8);
    column.append(&ui::label(tr("Search for:")));
    let entry = gtk::Entry::new();
    entry.set_widget_name("ofm-viewer-search-text");
    entry.set_hexpand(true);
    entry.update_property(&[gtk::accessible::Property::Label(tr("Search text"))]);
    column.append(&entry);

    let mode_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let text = radio(tr("_Text"), None);
    text.set_active(true);
    let hex = radio(tr("_Hex bytes"), Some(&text));
    let regex = radio(tr("_Regular expression"), Some(&text));
    for b in [&text, &hex, &regex] {
        mode_row.append(b);
    }
    column.append(&mode_row);

    let case = gtk::CheckButton::with_mnemonic(tr("_Case sensitive"));
    let backward = gtk::CheckButton::with_mnemonic(tr("_Backward"));
    let wrap = gtk::CheckButton::with_mnemonic(tr("_Wrap around"));
    let opts_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    for b in [&case, &backward, &wrap] {
        opts_row.append(b);
    }
    column.append(&opts_row);
    let error = ui::error_label();
    column.append(&error);
    let cancel = ui::button(tr("Cancel"));
    let go = ui::button(tr("_Search"));
    go.set_widget_name("ofm-viewer-search-run");
    go.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &go]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));

    // Defaults from the last search.
    wrap.set_active(true);
    if let Some(spec) = session.last_search() {
        case.set_active(spec.opts.case_sensitive);
        wrap.set_active(spec.opts.wrap);
        match &spec.needle {
            Needle::Text(t) => entry.set_text(t),
            Needle::Hex(b) => {
                hex.set_active(true);
                entry.set_text(&hex_text(b));
            }
            Needle::Regex(p) => {
                regex.set_active(true);
                entry.set_text(p);
            }
        }
        entry.select_region(0, -1);
    }
    {
        let error = error.clone();
        entry.connect_changed(move |_| ui::set_error(&error, None));
    }

    let submit: Rc<dyn Fn() -> bool> = {
        let (session, entry, error) = (Rc::clone(session), entry.clone(), error.clone());
        let (hex, regex, case, backward, wrap) = (
            hex.clone(),
            regex.clone(),
            case.clone(),
            backward.clone(),
            wrap.clone(),
        );
        Rc::new(move || {
            let text = entry.text().to_string();
            let needle = if hex.is_active() {
                match parse_hex(&text) {
                    Ok(b) => Needle::Hex(b),
                    Err(m) => {
                        ui::set_error(&error, Some(&m));
                        return false;
                    }
                }
            } else if regex.is_active() {
                Needle::Regex(text)
            } else {
                Needle::Text(text)
            };
            let opts = SearchOptions {
                case_sensitive: case.is_active(),
                wrap: wrap.is_active(),
            };
            // Validate (empty needle, bad regex) before closing the dialog.
            if let Err(e) = needle.clone().prepare(session.encoding(), &opts) {
                ui::set_error(&error, Some(&e.to_string()));
                return false;
            }
            let dir = if backward.is_active() {
                Dir::Backward
            } else {
                Dir::Forward
            };
            session.search(SearchSpec { needle, opts }, dir);
            true
        })
    };
    finish(ctx, session, &dialog, &cancel, &go, submit);
}

/// Alt+F8: go to a line, offset or percentage.
pub fn goto(ctx: &Rc<AppContext>, session: &Rc<Session>) {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Go to"));
    dialog.set_content_width(400);
    let column = modal::padded_column(8);
    column.append(&ui::label(tr("Position:")));
    let entry = gtk::Entry::new();
    entry.set_widget_name("ofm-viewer-goto-text");
    entry.update_property(&[gtk::accessible::Property::Label(tr("Position"))]);
    column.append(&entry);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let line = radio(tr("_Line number"), None);
    let offset = radio(tr("_Byte offset"), Some(&line));
    row.append(&line);
    row.append(&offset);
    if session.is_hex() {
        offset.set_active(true);
    }
    column.append(&row);
    column.append(&ui::caption(tr(
        "Use 0x for a hexadecimal offset and % for a percentage of the file.",
    )));
    let error = ui::error_label();
    column.append(&error);
    let cancel = ui::button(tr("Cancel"));
    let go = ui::button(tr("_Go"));
    go.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &go]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));
    {
        let error = error.clone();
        entry.connect_changed(move |_| ui::set_error(&error, None));
    }
    let submit: Rc<dyn Fn() -> bool> = {
        let (session, entry, error, offset) = (
            Rc::clone(session),
            entry.clone(),
            error.clone(),
            offset.clone(),
        );
        Rc::new(
            move || match parse_goto(&entry.text(), offset.is_active()) {
                Ok(t) => {
                    session.goto(t);
                    true
                }
                Err(m) => {
                    ui::set_error(&error, Some(&m));
                    false
                }
            },
        )
    };
    finish(ctx, session, &dialog, &cancel, &go, submit);
}

/// F8: choose the encoding (auto-detect or one of the supported ones).
pub fn encoding(ctx: &Rc<AppContext>, session: &Rc<Session>) {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Encoding"));
    dialog.set_content_width(340);
    let column = modal::padded_column(8);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Browse);
    list.set_widget_name("ofm-viewer-encodings");
    list.update_property(&[gtk::accessible::Property::Label(tr("Encodings"))]);
    let mut choices: Vec<Option<Encoding>> = vec![None];
    choices.extend(Encoding::ALL.iter().copied().map(Some));
    let current = session.encoding_choice();
    let mut selected = None;
    for (i, c) in choices.iter().enumerate() {
        let label = match c {
            None => tr("Auto-detect").to_owned(),
            Some(e) => e.label().to_owned(),
        };
        let row = gtk::ListBoxRow::new();
        let l = gtk::Label::new(Some(&label));
        l.set_xalign(0.0);
        l.set_margin_top(4);
        l.set_margin_bottom(4);
        l.set_margin_start(8);
        l.set_margin_end(8);
        row.set_child(Some(&l));
        row.update_property(&[gtk::accessible::Property::Label(&label)]);
        list.append(&row);
        if *c == current {
            selected = Some(row.clone());
        }
        let _ = i;
    }
    column.append(&list);
    let cancel = ui::button(tr("Cancel"));
    let ok = ui::button(tr("_OK"));
    ok.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &ok]));
    dialog.set_child(Some(&modal::with_header(&column)));
    if let Some(row) = &selected {
        list.select_row(Some(row));
        dialog.set_focus(Some(row));
    }
    let submit: Rc<dyn Fn() -> bool> = {
        let (session, list) = (Rc::clone(session), list.clone());
        Rc::new(move || {
            let Some(row) = list.selected_row() else {
                return true;
            };
            let idx = usize::try_from(row.index()).unwrap_or(0);
            if let Some(choice) = choices.get(idx) {
                session.set_encoding(*choice);
            }
            true
        })
    };
    {
        // A double click / Enter on a row applies it right away.
        let (submit, weak) = (Rc::clone(&submit), dialog.downgrade());
        list.connect_row_activated(move |_, _| {
            if submit()
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
        });
    }
    finish(ctx, session, &dialog, &cancel, &ok, submit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_text_roundtrips_with_the_parser() {
        let t = hex_text(&[0x48, 0x0A, 0xFF]);
        assert_eq!(t, "48 0A FF");
        assert_eq!(parse_hex(&t).unwrap_or_default(), [0x48, 0x0A, 0xFF]);
    }
}
