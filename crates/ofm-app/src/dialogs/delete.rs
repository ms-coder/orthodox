//! Delete confirmation (T3.3 owns this file; F14): names/counts, "Move to Trash" vs "Delete
//! permanently" (`permanent == true` for Shift+Del). Submits `JobSpec::Delete`. A trash that is
//! unavailable for an item is asked about by the engine (`Question::TrashUnavailable`, see
//! `dialogs::conflict`). `[general] confirm_delete = false` skips this dialog for trashing
//! (permanent deletion is always confirmed).

use std::rc::Rc;

use adw::prelude::*;
use ofm_jobs::{JobSpec, RunMode};

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::targets::Targets;
use crate::jobs::{self, SubmitOpts, ui};
use crate::navigation::modal;

/// How many names the dialog lists before "and N more".
const LISTED: usize = 8;

/// Heading and button label for the two modes.
fn texts(permanent: bool, what: &str) -> (String, &'static str) {
    if permanent {
        (
            format!(
                "{} {what}? {}",
                tr("Delete permanently"),
                tr("This cannot be undone.")
            ),
            tr("_Delete"),
        )
    } else {
        (
            format!("{} {what} {}", tr("Move"), tr("to the Trash?")),
            tr("Move to _Trash"),
        )
    }
}

/// Permanent deletion of several items or of any folder cannot be undone and can be large: the
/// dialog then starts on Cancel, so a reflexive Enter does nothing. A single plain file keeps
/// "Delete" as the default. Moving to the Trash is recoverable and always defaults to the action.
pub fn default_is_cancel(permanent: bool, items: usize, has_dirs: bool) -> bool {
    permanent && (items > 1 || has_dirs)
}

/// Names for the dialog body: up to [`LISTED`] lines, then "... and N more".
pub fn name_list(names: &[String]) -> String {
    let mut lines: Vec<String> = names.iter().take(LISTED).cloned().collect();
    if names.len() > LISTED {
        lines.push(format!("... {} {}", names.len() - LISTED, tr("more")));
    }
    lines.join("\n")
}

/// Confirm and submit deletion of the active panel's selection.
pub fn present(ctx: &Rc<AppContext>, permanent: bool) {
    let Some(targets) = Targets::from_active(ctx) else {
        return;
    };
    let Some(paths) = targets.local_paths() else {
        ui::message(
            ctx,
            tr("Delete"),
            tr("Items inside archives are read-only and cannot be deleted."),
        );
        return;
    };
    let Some(controller) = jobs::controller(ctx) else {
        return;
    };
    let submit = {
        let (ctx, controller, origin) = (
            Rc::clone(ctx),
            Rc::clone(&controller),
            Rc::downgrade(&targets.controller),
        );
        let paths = paths.clone();
        move |to_trash: bool| {
            controller.submit_with(
                &ctx,
                JobSpec::Delete {
                    paths: paths.clone(),
                    to_trash,
                },
                RunMode::Queued,
                SubmitOpts {
                    origin: Some(origin.clone()),
                    on_finished: None,
                },
            );
        }
    };
    if !permanent && !ctx.config.borrow().general.confirm_delete {
        submit(true);
        return;
    }

    let what = targets.describe();
    let (heading_text, run_label) = texts(permanent, &what);
    let dialog = adw::Dialog::new();
    dialog.set_title(if permanent {
        tr("Delete permanently")
    } else {
        tr("Delete")
    });
    dialog.set_content_width(460);
    let column = modal::padded_column(8);
    let heading = gtk::Label::builder()
        .label(&heading_text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["title-4"])
        .build();
    heading.set_widget_name("ofm-delete-heading");
    column.append(&heading);
    let names: Vec<String> = targets
        .entries
        .iter()
        .map(|e| e.name.to_string_lossy().into_owned())
        .collect();
    if names.len() > 1 {
        let list = ui::label(&name_list(&names));
        list.set_selectable(true);
        column.append(&list);
    }
    if targets.has_dirs() {
        column.append(&ui::caption(tr(
            "Folders are removed together with everything inside them.",
        )));
    }
    let perm = gtk::CheckButton::with_mnemonic(tr("Delete _permanently (skip the Trash)"));
    perm.set_widget_name("ofm-delete-permanent");
    perm.set_active(permanent);
    column.append(&perm);

    let cancel = ui::button(tr("Cancel"));
    cancel.set_widget_name("ofm-delete-cancel");
    let run = ui::button(run_label);
    run.set_widget_name("ofm-delete-run");
    run.add_css_class("destructive-action");
    column.append(&ui::button_row(&[&cancel, &run]));
    dialog.set_child(Some(&modal::with_header(&column)));
    if default_is_cancel(permanent, paths.len(), targets.has_dirs()) {
        dialog.set_focus(Some(&cancel));
    } else {
        dialog.set_focus(Some(&run));
    }

    // The check box switches between the two modes; the texts follow.
    {
        let (heading, run, what, dialog) = (
            heading.clone(),
            run.clone(),
            what.clone(),
            dialog.downgrade(),
        );
        perm.connect_toggled(move |c| {
            let (h, label) = texts(c.is_active(), &what);
            heading.set_text(&h);
            run.set_label(label);
            if let Some(d) = dialog.upgrade() {
                d.set_title(if c.is_active() {
                    tr("Delete permanently")
                } else {
                    tr("Delete")
                });
            }
        });
    }
    let confirm: Rc<dyn Fn() -> bool> = {
        let perm = perm.clone();
        Rc::new(move || {
            submit(!perm.is_active());
            true
        })
    };
    let weak = dialog.downgrade();
    {
        let (confirm, weak) = (Rc::clone(&confirm), weak.clone());
        run.connect_clicked(move |_| {
            confirm();
            if let Some(d) = weak.upgrade() {
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
        if ui::activate_focused_button(&ctx_ok) {
            return false;
        }
        confirm()
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn long_lists_are_cut() {
        let few: Vec<String> = (0..3).map(|i| format!("f{i}")).collect();
        assert_eq!(name_list(&few), "f0\nf1\nf2");
        let many: Vec<String> = (0..11).map(|i| format!("f{i}")).collect();
        let text = name_list(&many);
        assert_eq!(text.lines().count(), LISTED + 1);
        assert!(text.ends_with("... 3 more"));
    }

    #[test]
    fn dangerous_permanent_deletes_default_to_cancel() {
        assert!(!default_is_cancel(true, 1, false), "single plain file");
        assert!(default_is_cancel(true, 1, true), "a folder");
        assert!(default_is_cancel(true, 2, false), "several items");
        assert!(!default_is_cancel(false, 5, true), "trash is recoverable");
    }

    #[test]
    fn headings() {
        assert_eq!(texts(false, "'a'").0, "Move 'a' to the Trash?");
        assert!(
            texts(true, "3 items")
                .0
                .starts_with("Delete permanently 3 items?")
        );
    }
}
