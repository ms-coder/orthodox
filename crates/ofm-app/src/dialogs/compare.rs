//! Options dialog of "compare directories" (Shift+F2, F27). Owner: T8.1c.
//!
//! Everything is reachable by keyboard: Enter starts the comparison, Esc cancels, Tab walks the
//! fields, arrow keys switch inside a radio group and every option has an Alt+letter mnemonic.
//! (Space is bound to a job command in the `dialog` keymap section, so check boxes are toggled
//! with their mnemonics instead.)

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use ofm_core::compare::CompareMode;

use crate::compare::options::{self, CompareSettings, MarkFilter};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;

/// Radio button with a mnemonic and an accessible name without the underscore.
fn radio(label: &str, name: &str, group: Option<&gtk::CheckButton>) -> gtk::CheckButton {
    let b = gtk::CheckButton::with_mnemonic(label);
    b.set_widget_name(name);
    b.set_group(group);
    b.update_property(&[gtk::accessible::Property::Label(&label.replace('_', ""))]);
    b
}

/// Show the dialog, starting from `initial`. `on_accept` receives the validated settings when
/// the user confirms (Enter or the Compare button).
pub fn present(
    ctx: &Rc<AppContext>,
    initial: &CompareSettings,
    on_accept: impl Fn(CompareSettings) + 'static,
) {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Compare directories"));
    dialog.set_content_width(460);
    let column = modal::padded_column(6);

    // Mode.
    column.append(&ui::label(tr("Compare by:")));
    let modes: [(CompareMode, &str, &str); 4] = [
        (
            CompareMode::NameSizeMtime,
            tr("_Name, size and time"),
            "ofm-compare-mode-time",
        ),
        (
            CompareMode::NameOnly,
            tr("Name _only"),
            "ofm-compare-mode-name",
        ),
        (
            CompareMode::NameSize,
            tr("Name and si_ze"),
            "ofm-compare-mode-size",
        ),
        (
            CompareMode::Content,
            tr("_Content"),
            "ofm-compare-mode-content",
        ),
    ];
    let mut mode_buttons: Vec<(CompareMode, gtk::CheckButton)> = Vec::new();
    for (mode, label, name) in modes {
        let b = radio(label, name, mode_buttons.first().map(|(_, b)| b));
        b.set_margin_start(12);
        b.set_active(mode == initial.mode);
        column.append(&b);
        mode_buttons.push((mode, b));
    }

    // Names and directories.
    let case = gtk::CheckButton::with_mnemonic(tr("Case-_insensitive names"));
    case.set_widget_name("ofm-compare-case");
    case.update_property(&[gtk::accessible::Property::Label(tr(
        "Case-insensitive names",
    ))]);
    case.set_active(initial.case_insensitive);
    column.append(&case);
    let dirs =
        gtk::CheckButton::with_mnemonic(tr("Include _subdirectories (compared by name only)"));
    dirs.set_widget_name("ofm-compare-dirs");
    dirs.update_property(&[gtk::accessible::Property::Label(tr(
        "Include subdirectories",
    ))]);
    dirs.set_active(initial.include_dirs);
    column.append(&dirs);

    // Tolerance.
    let tol_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let tol_label = gtk::Label::with_mnemonic(tr("Time _tolerance (seconds):"));
    tol_label.set_xalign(0.0);
    let tol = gtk::Entry::new();
    tol.set_widget_name("ofm-compare-tolerance");
    tol.set_width_chars(8);
    tol.set_text(&options::format_tolerance(initial.tolerance));
    tol.update_property(&[gtk::accessible::Property::Label(tr(
        "Time tolerance in seconds",
    ))]);
    tol_label.set_mnemonic_widget(Some(&tol));
    tol_row.append(&tol_label);
    tol_row.append(&tol);
    column.append(&tol_row);
    let error = ui::error_label();
    column.append(&error);

    // Which differences get marked.
    column.append(&ui::label(tr("Mark:")));
    let marks: [(MarkFilter, &str, &str); 3] = [
        (
            MarkFilter::AllDiffering,
            tr("_All differing"),
            "ofm-compare-mark-all",
        ),
        (
            MarkFilter::NewerHere,
            tr("Only n_ewer here"),
            "ofm-compare-mark-newer",
        ),
        (
            MarkFilter::MissingOnOther,
            tr("Only _missing on the other side"),
            "ofm-compare-mark-missing",
        ),
    ];
    let mut mark_buttons: Vec<(MarkFilter, gtk::CheckButton)> = Vec::new();
    for (filter, label, name) in marks {
        let b = radio(label, name, mark_buttons.first().map(|(_, b)| b));
        b.set_margin_start(12);
        b.set_active(filter == initial.mark);
        column.append(&b);
        mark_buttons.push((filter, b));
    }

    // The tolerance only matters for the time comparison.
    let sync_tolerance = {
        let (tol, tol_label) = (tol.clone(), tol_label.clone());
        let time = mode_buttons[0].1.clone();
        move || {
            tol.set_sensitive(time.is_active());
            tol_label.set_sensitive(time.is_active());
        }
    };
    sync_tolerance();
    mode_buttons[0].1.connect_toggled(move |_| sync_tolerance());

    let cancel = ui::button(tr("Cancel"));
    cancel.set_widget_name("ofm-compare-cancel");
    let run = ui::button(tr("Compare"));
    run.set_widget_name("ofm-compare-run");
    run.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &run]));
    dialog.set_child(Some(&modal::with_header(&column)));
    // The checked mode radio has the focus: arrows switch the mode, Enter starts the comparison.
    if let Some((_, b)) = mode_buttons.iter().find(|(m, _)| *m == initial.mode) {
        dialog.set_focus(Some(b));
    }

    let confirm: Rc<dyn Fn() -> bool> = {
        let error = error.clone();
        Rc::new(move || {
            let mode = mode_buttons
                .iter()
                .find(|(_, b)| b.is_active())
                .map_or(initial_mode(), |(m, _)| *m);
            let mark = mark_buttons
                .iter()
                .find(|(_, b)| b.is_active())
                .map_or(MarkFilter::AllDiffering, |(m, _)| *m);
            let tolerance = if mode == CompareMode::NameSizeMtime {
                match options::parse_tolerance(&tol.text()) {
                    Ok(d) => d,
                    Err(msg) => {
                        ui::set_error(&error, Some(&msg));
                        tol.add_css_class("error");
                        tol.grab_focus();
                        return false;
                    }
                }
            } else {
                // Not shown as active: keep whatever is typed when valid, else the default.
                options::parse_tolerance(&tol.text()).unwrap_or(Duration::from_secs(u64::from(
                    options::DEFAULT_TOLERANCE_SECS,
                )))
            };
            on_accept(CompareSettings {
                mode,
                case_insensitive: case.is_active(),
                include_dirs: dirs.is_active(),
                tolerance,
                mark,
            });
            true
        })
    };
    let weak = dialog.downgrade();
    {
        let (confirm, weak) = (Rc::clone(&confirm), weak.clone());
        run.connect_clicked(move |_| {
            if confirm()
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
    let ctx_ok = Rc::clone(ctx);
    modal::present(ctx, &dialog, move || {
        // Enter on the Cancel or Compare button presses that button instead.
        if ui::activate_focused_button(&ctx_ok) {
            return false;
        }
        confirm()
    });
}

/// Mode used when no radio button is active (cannot happen; kept total instead of unwrapping).
fn initial_mode() -> CompareMode {
    CompareSettings::default().mode
}
