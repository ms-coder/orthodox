//! The F2 user menu dialog (F28): the entries (local menu first), Up/Down + Enter, one hotkey
//! letter per entry, Esc to close, buttons to edit the menu files, and a collapsible
//! "N problems in menu files" row.
//!
//! Keys go through the central dispatcher like in every dialog (`modal::present`): Enter is
//! `dialog.ok` (runs the selected entry), Esc `dialog.cancel`, Up/Down are not bound and move
//! the list natively; the entry hotkeys are dynamic and therefore use the dispatcher's
//! [`DialogKeyFallback`] hook. The chosen action runs after the dialog has closed.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use ofm_core::{CommandId, Key, KeyChord, cmd};

use crate::context::AppContext;
use crate::dispatch::DialogKeyFallback;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;
use crate::usermenu::load::{MenuView, ShownEntry};

/// What the dialog was closed for.
enum Chosen {
    Run(Box<ShownEntry>),
    Command(CommandId),
    Review,
}

/// Callbacks of the dialog owner.
pub struct MenuActions {
    /// The user picked an entry (called after the dialog closed).
    pub on_run: Rc<dyn Fn(ShownEntry)>,
    /// The user asked to review the ignored local menu again (after close).
    pub on_review: Rc<dyn Fn()>,
}

/// Whether `chord` is the plain press of the hotkey `entry` declares (Shift allowed: a capital
/// letter; Ctrl/Alt/Super never).
pub fn hotkey_matches(entry: &ShownEntry, chord: KeyChord) -> bool {
    let m = chord.mods;
    if m.ctrl || m.alt || m.super_ {
        return false;
    }
    match (chord.key, entry.hotkey()) {
        (Key::Char(c), Some(h)) => c.to_uppercase().eq(h.to_uppercase()),
        _ => false,
    }
}

/// Index of the entry whose hotkey `chord` presses.
pub fn entry_for_chord(entries: &[ShownEntry], chord: KeyChord) -> Option<usize> {
    entries.iter().position(|e| hotkey_matches(e, chord))
}

fn row_for(entry: &ShownEntry) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.set_margin_top(4);
    line.set_margin_bottom(4);
    line.set_margin_start(8);
    line.set_margin_end(8);
    let key = gtk::Label::builder()
        .label(entry.hotkey().map(String::from).unwrap_or_default())
        .width_chars(2)
        .xalign(0.5)
        .css_classes(["monospace", "heading"])
        .build();
    line.append(&key);
    line.append(
        &gtk::Label::builder()
            .label(&entry.item.title)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    if entry.local {
        line.append(
            &gtk::Label::builder()
                .label(tr("this folder"))
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    row.set_child(Some(&line));
    row.update_property(&[gtk::accessible::Property::Label(&entry.item.title)]);
    row
}

/// "N problems in menu files".
pub fn problems_title(n: usize) -> String {
    if n == 1 {
        tr("1 problem in menu files").to_owned()
    } else {
        format!("{n} {}", tr("problems in menu files"))
    }
}

/// Show the menu. Returns the dialog (`None` when it could not be shown).
pub fn present(ctx: &Rc<AppContext>, view: &MenuView, actions: MenuActions) -> Option<adw::Dialog> {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("User menu"));
    dialog.set_content_width(480);
    let column = modal::padded_column(8);
    let entries: Rc<Vec<ShownEntry>> = Rc::new(view.entries.clone());
    let chosen: Rc<RefCell<Option<Chosen>>> = Rc::default();

    let list = gtk::ListBox::new();
    list.set_widget_name("ofm-usermenu-list");
    list.set_selection_mode(gtk::SelectionMode::Browse);
    list.add_css_class("boxed-list");
    list.update_property(&[gtk::accessible::Property::Label(tr("User menu entries"))]);
    for e in entries.iter() {
        list.append(&row_for(e));
    }
    if entries.is_empty() {
        let empty = ui::caption(tr("No entries. Use \"Edit menu\" to add some."));
        empty.set_widget_name("ofm-usermenu-empty");
        column.append(&empty);
    } else {
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .min_content_height(80)
            .max_content_height(420)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        column.append(&scroll);
    }

    let close_with = |chosen_value: Chosen| {
        let chosen = Rc::clone(&chosen);
        let weak = dialog.downgrade();
        move |_: &gtk::Button| {
            *chosen.borrow_mut() = Some(chosen_value_clone(&chosen_value));
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        }
    };

    if view.local_ignored {
        let note = ui::caption(tr(
            "A menu file in this folder is not trusted and was ignored.",
        ));
        note.set_widget_name("ofm-usermenu-ignored");
        column.append(&note);
        let review = ui::button(tr("_Review local menu"));
        review.set_widget_name("ofm-usermenu-review");
        review.connect_clicked(close_with(Chosen::Review));
        column.append(&review);
    }

    if !view.problems.is_empty() {
        let toggle = gtk::Button::with_label(&problems_title(view.problems.len()));
        toggle.set_widget_name("ofm-usermenu-problems");
        toggle.add_css_class("flat");
        toggle.set_halign(gtk::Align::Start);
        toggle.update_property(&[gtk::accessible::Property::Label(&problems_title(
            view.problems.len(),
        ))]);
        let details = gtk::Label::builder()
            .label(view.problems.join("\n"))
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .max_width_chars(64)
            .css_classes(["caption", "dim-label"])
            .build();
        details.set_widget_name("ofm-usermenu-problem-details");
        let revealer = gtk::Revealer::builder().child(&details).build();
        {
            let revealer = revealer.clone();
            toggle.connect_clicked(move |_| revealer.set_reveal_child(!revealer.reveals_child()));
        }
        column.append(&toggle);
        column.append(&revealer);
    }

    let edit = ui::button(tr("Edit _menu"));
    edit.set_widget_name("ofm-usermenu-edit");
    let edit_local = ui::button(tr("Edit _local menu"));
    edit_local.set_widget_name("ofm-usermenu-edit-local");
    let close = ui::button(tr("_Close"));
    edit.connect_clicked(close_with(Chosen::Command(cmd::MENU_EDIT)));
    edit_local.connect_clicked(close_with(Chosen::Command(cmd::MENU_EDIT_LOCAL)));
    {
        let weak = dialog.downgrade();
        close.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    column.append(&ui::button_row(&[&edit, &edit_local, &close]));
    dialog.set_child(Some(&modal::with_header(&column)));

    // Focus: the first entry (arrow keys and Enter work at once), else the edit button.
    match list.row_at_index(0) {
        Some(first) if !entries.is_empty() => {
            list.select_row(Some(&first));
            dialog.set_focus(Some(&first));
        }
        _ => dialog.set_focus(Some(&edit)),
    }

    // Mouse / activation of a row.
    {
        let (entries, chosen, weak) = (Rc::clone(&entries), Rc::clone(&chosen), dialog.downgrade());
        list.connect_row_activated(move |_, row| {
            let picked = usize::try_from(row.index())
                .ok()
                .and_then(|i| entries.get(i).cloned());
            if let Some(e) = picked {
                *chosen.borrow_mut() = Some(Chosen::Run(Box::new(e)));
                if let Some(d) = weak.upgrade() {
                    d.close();
                }
            }
        });
    }

    // Hotkey letters (dynamic, so not in the keymap): the dispatcher offers unbound chords.
    let fallback = DialogKeyFallback::ensure(ctx);
    {
        let (entries, chosen, weak) = (Rc::clone(&entries), Rc::clone(&chosen), dialog.downgrade());
        fallback.set(Rc::new(move |_ctx, chord| {
            let Some(i) = entry_for_chord(&entries, chord) else {
                return false;
            };
            *chosen.borrow_mut() = Some(Chosen::Run(Box::new(entries[i].clone())));
            if let Some(d) = weak.upgrade() {
                d.close();
            }
            true
        }));
    }

    {
        let (ctx, chosen) = (Rc::clone(ctx), Rc::clone(&chosen));
        dialog.connect_closed(move |_| {
            DialogKeyFallback::ensure(&ctx).clear();
            let what = chosen.borrow_mut().take();
            let (ctx, on_run, on_review) = (
                Rc::clone(&ctx),
                Rc::clone(&actions.on_run),
                Rc::clone(&actions.on_review),
            );
            // After the dialog is really gone (the next dialog must not stack on a closing one).
            gtk::glib::idle_add_local_once(move || match what {
                Some(Chosen::Run(e)) => on_run(*e),
                Some(Chosen::Review) => on_review(),
                Some(Chosen::Command(c)) => {
                    ctx.execute(c);
                }
                None => {}
            });
        });
    }

    let (ctx_ok, list_ok, entries_ok, chosen_ok) = (
        Rc::clone(ctx),
        list.clone(),
        Rc::clone(&entries),
        Rc::clone(&chosen),
    );
    let shown = modal::present(ctx, &dialog, move || {
        // Enter on a focused button presses that button.
        if ui::activate_focused_button(&ctx_ok) {
            return false;
        }
        let picked = list_ok
            .selected_row()
            .and_then(|r| usize::try_from(r.index()).ok())
            .and_then(|i| entries_ok.get(i).cloned());
        match picked {
            Some(e) => {
                *chosen_ok.borrow_mut() = Some(Chosen::Run(Box::new(e)));
                true
            }
            None => false,
        }
    });
    if shown {
        Some(dialog)
    } else {
        fallback.clear();
        None
    }
}

fn chosen_value_clone(c: &Chosen) -> Chosen {
    match c {
        Chosen::Run(e) => Chosen::Run(e.clone()),
        Chosen::Command(id) => Chosen::Command(*id),
        Chosen::Review => Chosen::Review,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ofm_config::menu::MenuItem;
    use ofm_core::Mods;

    fn entry(key: &str, title: &str) -> ShownEntry {
        ShownEntry {
            item: MenuItem {
                key: key.into(),
                title: title.into(),
                command: "true".into(),
                ..MenuItem::default()
            },
            local: false,
        }
    }

    fn chord(s: &str) -> KeyChord {
        s.parse().unwrap()
    }

    #[test]
    fn hotkeys_match_case_insensitively_without_ctrl_or_alt() {
        let entries = vec![entry("t", "tar"), entry("R", "rsync"), entry("", "none")];
        assert_eq!(entry_for_chord(&entries, chord("T")), Some(0));
        assert_eq!(entry_for_chord(&entries, chord("Shift+R")), Some(1));
        assert_eq!(entry_for_chord(&entries, chord("Ctrl+T")), None);
        assert_eq!(entry_for_chord(&entries, chord("Alt+R")), None);
        assert_eq!(entry_for_chord(&entries, chord("X")), None);
        assert_eq!(entry_for_chord(&entries, chord("Enter")), None);
        let _ = Mods::default();
    }

    #[test]
    fn problems_title_pluralises() {
        assert_eq!(problems_title(1), "1 problem in menu files");
        assert_eq!(problems_title(3), "3 problems in menu files");
    }
}
