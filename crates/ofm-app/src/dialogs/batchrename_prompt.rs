//! The small prompts of the batch rename flow: waiting for a detached editor, a problem with the
//! edited text, and the offer to roll back a batch that stopped half way. All are two-button
//! dialogs on [`super::batchrename_choice`]: Enter = the first action, Esc = cancel.

use std::rc::Rc;

use adw::prelude::*;

use super::batchrename_choice::{self, ButtonSpec, ChoiceSpec};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;

fn label_widget(text: &str, name: &str) -> gtk::Widget {
    let l = ui::label(text);
    l.set_selectable(true);
    l.set_max_width_chars(70);
    l.set_widget_name(name);
    l.upcast()
}

/// The editor was started detached (a GUI editor): the user edits and comes back. `on_answer(true)`
/// = Apply (read the file now), `false` = Cancel.
pub fn waiting(ctx: &Rc<AppContext>, on_answer: impl FnOnce(bool) + 'static) {
    batchrename_choice::present(
        ctx,
        ChoiceSpec {
            title: tr("Batch rename"),
            width: 420,
            content: vec![
                label_widget(tr("Waiting for your editor..."), "ofm-batchrename-waiting"),
                ui::caption(tr(
                    "Edit the names (one per line, keep the order), save the file, then press Apply.",
                ))
                .upcast(),
            ],
            buttons: vec![
                ButtonSpec {
                    label: tr("Cancel"),
                    name: "ofm-batchrename-cancel",
                    suggested: false,
                },
                ButtonSpec {
                    label: tr("_Apply"),
                    name: "ofm-batchrename-apply",
                    suggested: true,
                },
            ],
            default: 1,
            cancel: 0,
        },
        move |i| on_answer(i == 1),
    );
}

/// The edited text cannot be applied. `on_answer(true)` = open the editor again on the same
/// (edited) file, `false` = cancel the whole batch.
pub fn problem(ctx: &Rc<AppContext>, text: &str, on_answer: impl FnOnce(bool) + 'static) {
    batchrename_choice::present(
        ctx,
        ChoiceSpec {
            title: tr("Batch rename"),
            width: 480,
            content: vec![
                ui::label(tr("The new names cannot be applied:")).upcast(),
                label_widget(text, "ofm-batchrename-problem"),
                ui::caption(tr("Nothing has been renamed.")).upcast(),
            ],
            buttons: vec![
                ButtonSpec {
                    label: tr("Cancel"),
                    name: "ofm-batchrename-cancel",
                    suggested: false,
                },
                ButtonSpec {
                    label: tr("_Re-open editor"),
                    name: "ofm-batchrename-reopen",
                    suggested: true,
                },
            ],
            default: 1,
            cancel: 0,
        },
        move |i| on_answer(i == 1),
    );
}

/// A batch stopped after `done` renames. `on_answer(true)` = roll back what was done,
/// `false` = keep it (it can still be undone later with the undo command).
pub fn rollback_offer(
    ctx: &Rc<AppContext>,
    error: &str,
    done: u64,
    on_answer: impl FnOnce(bool) + 'static,
) {
    let body = format!(
        "{error}\n\n{done} {}",
        tr("rename step(s) were already done.")
    );
    batchrename_choice::present(
        ctx,
        ChoiceSpec {
            title: tr("Batch rename stopped"),
            width: 480,
            content: vec![
                label_widget(&body, "ofm-batchrename-stopped"),
                ui::caption(tr(
                    "Roll back restores the original names. Keep leaves the folder as it is now.",
                ))
                .upcast(),
            ],
            buttons: vec![
                ButtonSpec {
                    label: tr("_Keep"),
                    name: "ofm-batchrename-keep",
                    suggested: false,
                },
                ButtonSpec {
                    label: tr("_Roll back what was done"),
                    name: "ofm-batchrename-rollback",
                    suggested: true,
                },
            ],
            default: 1,
            cancel: 0,
        },
        move |i| on_answer(i == 1),
    );
}
