//! Question dialog (T3.3 owns this file; F16): one dialog for every `ofm_jobs::Question`:
//! `Conflict` (both sides' size/date, choices overwrite / skip / rename new / rename existing /
//! overwrite if newer / overwrite if size differs / compare, "apply to all"; honours
//! `ConflictKind::allows` by disabling forbidden choices), `Error` (retry / skip / abort, path +
//! message + errno hint, "apply to all" for skip/abort) and `TrashUnavailable` (delete
//! permanently / skip / abort). Answers through `reply.answer(..)` exactly once; closing the
//! dialog without an answer drops the responder, which aborts the job. Holds `ctx.enter_modal()`
//! while open (through `modal::present`).
//!
//! Keys: Enter presses the focused button, or the safe default (Skip; "Rename new" when the
//! conflict is with the file itself or with a different kind of item; Retry for errors), Esc
//! aborts, Ctrl+A toggles "apply to all". Every choice also has an Alt+mnemonic.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use ofm_core::Responder;
use ofm_jobs::{
    Answer, ConflictAnswer, ConflictKind, ErrorAction, ItemInfo, ItemKind, JobId, Operation,
    Question, RealFs,
};

use crate::chrome::statusline::{SizeFormat, format_size, format_time};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::actions::{self, Action};
use crate::jobs::compare::{Comparison, compare_files};
use crate::jobs::ui;
use crate::navigation::modal;
use crate::wiring;

/// Show the dialog for `question` of job `id`.
pub fn present(ctx: &Rc<AppContext>, id: JobId, question: Question, reply: Responder<Answer>) {
    let _ = present_with(ctx, id, question, reply, Box::new(|| {}));
}

/// Like [`present`]; `on_closed` runs once when the dialog is gone (answered or dismissed).
/// Returns the dialog, or `None` when it cannot be shown (no main window): the responder is then
/// dropped, which aborts the job, and `on_closed` is not called.
pub fn present_with(
    ctx: &Rc<AppContext>,
    id: JobId,
    question: Question,
    reply: Responder<Answer>,
    on_closed: Box<dyn FnOnce()>,
) -> Option<adw::Dialog> {
    let slot: Rc<RefCell<Option<Responder<Answer>>>> = Rc::new(RefCell::new(Some(reply)));
    let dialog = adw::Dialog::new();
    dialog.set_content_width(580);
    tracing::debug!(job = id.0, "question dialog");

    // Answering sends the answer once and closes the dialog (weak: the buttons live inside it).
    let weak = dialog.downgrade();
    let answer: Rc<dyn Fn(Option<Answer>)> = {
        let slot = Rc::clone(&slot);
        Rc::new(move |a| {
            let reply = slot.borrow_mut().take();
            if let (Some(reply), Some(a)) = (reply, a) {
                reply.answer(a);
            }
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        })
    };

    let ui = match question {
        Question::Conflict {
            source,
            existing,
            kind,
        } => build_conflict(
            &ctx.services.pool,
            &dialog,
            &source,
            &existing,
            kind,
            &answer,
        ),
        Question::Error {
            path,
            operation,
            message,
            os_error,
        } => build_error(
            &dialog,
            &path.to_string_lossy(),
            operation,
            &message,
            os_error,
            &answer,
        ),
        Question::TrashUnavailable { path } => {
            build_trash(&dialog, &path.to_string_lossy(), &answer)
        }
    };
    let Built {
        column,
        focus,
        default_action,
        apply_all,
        cleanup,
    } = ui;
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&focus));

    let ctx_ok = Rc::clone(ctx);
    let shown = modal::present(ctx, &dialog, move || {
        if !ui::activate_focused_button(&ctx_ok) {
            default_action();
        }
        false // the answer closes the dialog
    });
    if !shown {
        return None; // `slot` is dropped with the closures: the job aborts
    }
    actions::bind(ctx, &dialog, move |a| match (a, &apply_all) {
        (Action::ApplyAll, Some(c)) => {
            c.set_active(!c.is_active());
            true
        }
        _ => false,
    });
    let on_closed = RefCell::new(Some(on_closed));
    dialog.connect_closed(move |_| {
        slot.borrow_mut().take(); // unanswered: drop = abort
        cleanup();
        if let Some(f) = on_closed.borrow_mut().take() {
            f();
        }
    });
    Some(dialog)
}

/// Widgets and behaviour of one question variant.
struct Built {
    column: gtk::Box,
    /// Initial focus (the safe default button).
    focus: gtk::Widget,
    /// What Enter does when no button is focused.
    default_action: Box<dyn Fn()>,
    /// The "apply to all" check box, if the dialog has one.
    apply_all: Option<gtk::CheckButton>,
    /// Runs when the dialog closes (stops background comparison).
    cleanup: Box<dyn Fn()>,
}

fn apply_all_check(text: &str) -> gtk::CheckButton {
    let c = gtk::CheckButton::with_mnemonic(text);
    c.update_property(&[gtk::accessible::Property::Label(&text.replace('_', ""))]);
    c
}

// ---- conflict ---------------------------------------------------------------------------------------

/// `1.5 MiB (1 572 864 B)`.
fn size_text(bytes: u64) -> String {
    format!(
        "{} ({} B)",
        format_size(bytes, SizeFormat::Binary),
        format_size(bytes, SizeFormat::Bytes)
    )
}

fn kind_text(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::File => tr("File"),
        ItemKind::Dir => tr("Folder"),
        ItemKind::Symlink => tr("Symbolic link"),
        ItemKind::Other => tr("Special file"),
    }
}

/// Which side is newer/larger, for the hints under each card.
fn compare_hints(source: &ItemInfo, existing: &ItemInfo) -> (Vec<String>, Vec<String>) {
    let (mut s, mut e) = (Vec::new(), Vec::new());
    match source.mtime_ns.cmp(&existing.mtime_ns) {
        std::cmp::Ordering::Greater => {
            s.push(tr("newer").to_owned());
            e.push(tr("older").to_owned());
        }
        std::cmp::Ordering::Less => {
            s.push(tr("older").to_owned());
            e.push(tr("newer").to_owned());
        }
        std::cmp::Ordering::Equal => {
            s.push(tr("same time").to_owned());
            e.push(tr("same time").to_owned());
        }
    }
    match source.size.cmp(&existing.size) {
        std::cmp::Ordering::Greater => {
            s.push(tr("larger").to_owned());
            e.push(tr("smaller").to_owned());
        }
        std::cmp::Ordering::Less => {
            s.push(tr("smaller").to_owned());
            e.push(tr("larger").to_owned());
        }
        std::cmp::Ordering::Equal => {
            s.push(tr("same size").to_owned());
            e.push(tr("same size").to_owned());
        }
    }
    (s, e)
}

fn item_card(heading: &str, item: &ItemInfo, hints: &[String]) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
    card.set_hexpand(true);
    card.add_css_class("card");
    card.set_margin_top(2);
    card.set_margin_bottom(2);
    let inner = modal::padded_column(2);
    let head = gtk::Label::builder()
        .label(heading)
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    inner.append(&head);
    let name = item
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = item
        .path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = gtk::Label::builder()
        .label(&name)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .tooltip_text(item.path.to_string_lossy())
        .build();
    inner.append(&path);
    inner.append(
        &gtk::Label::builder()
            .label(&dir)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    let mut lines = vec![kind_text(item.kind).to_owned()];
    if item.kind == ItemKind::File {
        lines.push(size_text(item.size));
    }
    lines.push(format_time(item.mtime_ns, "%Y-%m-%d %H:%M:%S"));
    for l in lines {
        inner.append(&ui::label(&l));
    }
    if !hints.is_empty() {
        inner.append(&ui::caption(&hints.join(", ")));
    }
    card.append(&inner);
    card
}

fn build_conflict(
    pool: &ofm_vfs::pool::IoPool,
    dialog: &adw::Dialog,
    source: &ItemInfo,
    existing: &ItemInfo,
    kind: ConflictKind,
    answer: &Rc<dyn Fn(Option<Answer>)>,
) -> Built {
    dialog.set_title(tr("File exists"));
    let column = modal::padded_column(8);
    let name = existing
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    column.append(
        &gtk::Label::builder()
            .label(format!(
                "'{name}' {}",
                tr("already exists in the destination.")
            ))
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-4"])
            .build(),
    );
    match kind {
        ConflictKind::Replaceable => {}
        ConflictKind::TypeMismatch => column.append(&ui::caption(tr(
            "A file and a folder cannot replace each other. Keep both under a new name, skip it or abort.",
        ))),
        ConflictKind::SameFile => column.append(&ui::caption(tr(
            "The source and the destination are the same file. Keep both under a new name, skip it or abort.",
        ))),
    }
    let (hs, he) = compare_hints(source, existing);
    let cards = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    cards.set_homogeneous(true);
    cards.append(&item_card(tr("New (source)"), source, &hs));
    cards.append(&item_card(tr("Existing (destination)"), existing, &he));
    column.append(&cards);

    let apply_all = apply_all_check(tr("_Apply to all remaining conflicts"));
    apply_all.set_margin_top(4);
    column.append(&apply_all);

    // Choice buttons in two columns; forbidden ones are disabled (ConflictKind::allows).
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(6);
    grid.set_column_homogeneous(true);
    let choices: [(ConflictAnswer, &str); 7] = [
        (ConflictAnswer::Overwrite, tr("_Overwrite")),
        (ConflictAnswer::Skip, tr("_Skip")),
        (ConflictAnswer::RenameNew, tr("Rename _new (keep both)")),
        (ConflictAnswer::RenameExisting, tr("Rename _existing")),
        (ConflictAnswer::OverwriteIfNewer, tr("Overwrite if ne_wer")),
        (
            ConflictAnswer::OverwriteIfSizeDiffers,
            tr("Overwrite if si_ze differs"),
        ),
        (ConflictAnswer::Abort, tr("_Abort job")),
    ];
    let default_choice = if kind == ConflictKind::Replaceable {
        ConflictAnswer::Skip
    } else {
        ConflictAnswer::RenameNew
    };
    let mut default_button: Option<gtk::Button> = None;
    for (i, (choice, label)) in choices.iter().enumerate() {
        let b = ui::button(label);
        b.set_hexpand(true);
        b.set_widget_name(&format!("ofm-conflict-{choice:?}").to_lowercase());
        b.set_sensitive(kind.allows(*choice));
        if *choice == default_choice {
            b.add_css_class("suggested-action");
            default_button = Some(b.clone());
        }
        if *choice == ConflictAnswer::Overwrite {
            b.add_css_class("destructive-action");
        }
        let (answer, check, choice) = (Rc::clone(answer), apply_all.clone(), *choice);
        b.connect_clicked(move |_| {
            answer(Some(Answer::Conflict {
                action: choice,
                apply_to_all: check.is_active(),
            }));
        });
        grid.attach(&b, (i % 2) as i32, (i / 2) as i32, 1, 1);
    }

    // Compare (UI only): read both files on a worker and show the verdict here.
    let compare = ui::button(tr("_Compare contents"));
    compare.set_hexpand(true);
    compare.set_widget_name("ofm-conflict-compare");
    let both_files = source.kind == ItemKind::File && existing.kind == ItemKind::File;
    compare.set_sensitive(both_files);
    let verdict = ui::caption("");
    verdict.set_visible(false);
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let (src, dst) = (source.path.clone(), existing.path.clone());
        let (verdict, cancel) = (verdict.clone(), Arc::clone(&cancel));
        let pool = pool.clone();
        compare.connect_clicked(move |button| {
            button.set_sensitive(false);
            verdict.set_text(tr("Comparing..."));
            verdict.set_visible(true);
            let flag = Arc::clone(&cancel);
            let (src, dst) = (src.clone(), dst.clone());
            let rx = wiring::run_on_pool(&pool, move |tx| {
                let r = compare_files(&RealFs, &src, &dst, &flag);
                let _ = tx.send_blocking(r);
            });
            let (verdict, button) = (verdict.clone(), button.downgrade());
            wiring::attach(
                rx,
                move |r| {
                    verdict.set_text(&match r {
                        Ok(Comparison::Identical) => tr("The contents are identical.").to_owned(),
                        Ok(Comparison::DiffersAt(at)) => {
                            format!(
                                "{} {at}.",
                                tr("The contents differ; first difference at byte")
                            )
                        }
                        Ok(Comparison::Cancelled) => String::new(),
                        Err(e) => format!("{}: {e}", tr("Cannot compare")),
                    });
                    if let Some(b) = button.upgrade() {
                        b.set_sensitive(true);
                    }
                    std::ops::ControlFlow::Break(())
                },
                || {},
            );
        });
    }
    grid.attach(&compare, 1, 3, 1, 1);
    column.append(&grid);
    column.append(&verdict);

    let default_action: Box<dyn Fn()> = {
        let (answer, check) = (Rc::clone(answer), apply_all.clone());
        Box::new(move || {
            answer(Some(Answer::Conflict {
                action: default_choice,
                apply_to_all: check.is_active(),
            }));
        })
    };
    let focus = default_button.map_or_else(|| grid.clone().upcast::<gtk::Widget>(), |b| b.upcast());
    Built {
        column,
        focus,
        default_action,
        apply_all: Some(apply_all),
        cleanup: Box::new(move || cancel.store(true, Ordering::Relaxed)),
    }
}

// ---- error ------------------------------------------------------------------------------------------

/// Short advice for common OS error numbers (Linux values).
pub fn errno_hint(os_error: Option<i32>) -> Option<&'static str> {
    Some(match os_error? {
        1 | 13 => tr("Permission denied. Check the permissions of the file and its folder."),
        2 => tr("The file or folder no longer exists."),
        5 => tr("Input/output error. The device may be failing; data may be lost."),
        17 => tr("An item with this name already exists."),
        18 => tr("The items are on different file systems."),
        20 => tr("A path component is not a folder."),
        21 => tr("The item is a folder."),
        28 => tr("No space left on the destination. Free some space, then choose Retry."),
        30 => tr("The file system is read-only."),
        36 => tr("The file name is too long for this file system."),
        122 => tr("The disk quota is exceeded."),
        _ => return None,
    })
}

fn build_error(
    dialog: &adw::Dialog,
    path: &str,
    operation: Operation,
    message: &str,
    os_error: Option<i32>,
    answer: &Rc<dyn Fn(Option<Answer>)>,
) -> Built {
    dialog.set_title(tr("Error"));
    let column = modal::padded_column(8);
    column.append(
        &gtk::Label::builder()
            .label(format!("{} {operation}", tr("Could not")))
            .xalign(0.0)
            .css_classes(["title-4"])
            .build(),
    );
    let p = ui::label(path);
    p.set_selectable(true);
    p.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    p.set_wrap(false);
    column.append(&p);
    let m = ui::label(message);
    m.set_selectable(true);
    m.add_css_class("ofm-error");
    column.append(&m);
    if let Some(h) = errno_hint(os_error) {
        column.append(&ui::caption(h));
    }
    let apply_all = apply_all_check(tr("Remember Skip/Abort for _all further errors"));
    column.append(&apply_all);

    let retry = ui::button(tr("_Retry"));
    retry.add_css_class("suggested-action");
    let skip = ui::button(tr("_Skip"));
    let abort = ui::button(tr("_Abort"));
    retry.set_widget_name("ofm-error-retry");
    skip.set_widget_name("ofm-error-skip");
    abort.set_widget_name("ofm-error-abort");
    for (b, action) in [
        (&retry, ErrorAction::Retry),
        (&skip, ErrorAction::Skip),
        (&abort, ErrorAction::Abort),
    ] {
        let (answer, check) = (Rc::clone(answer), apply_all.clone());
        b.connect_clicked(move |_| {
            // A remembered retry would loop forever (ADR-0006): only skip/abort are remembered.
            let all = check.is_active() && action != ErrorAction::Retry;
            answer(Some(Answer::Error {
                action,
                apply_to_all: all,
            }));
        });
    }
    column.append(&ui::button_row(&[&abort, &skip, &retry]));
    let default_action: Box<dyn Fn()> = {
        let answer = Rc::clone(answer);
        Box::new(move || {
            answer(Some(Answer::Error {
                action: ErrorAction::Retry,
                apply_to_all: false,
            }));
        })
    };
    Built {
        column,
        focus: retry.upcast(),
        default_action,
        apply_all: Some(apply_all),
        cleanup: Box::new(|| {}),
    }
}

// ---- trash unavailable --------------------------------------------------------------------------------

fn build_trash(dialog: &adw::Dialog, path: &str, answer: &Rc<dyn Fn(Option<Answer>)>) -> Built {
    dialog.set_title(tr("Cannot move to Trash"));
    let column = modal::padded_column(8);
    column.append(
        &gtk::Label::builder()
            .label(tr("This item cannot be moved to the Trash."))
            .xalign(0.0)
            .css_classes(["title-4"])
            .build(),
    );
    let p = ui::label(path);
    p.set_selectable(true);
    p.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    p.set_wrap(false);
    column.append(&p);
    column.append(&ui::caption(tr(
        "The Trash is not available for this location (for example a network or removable drive). Delete it permanently instead? This cannot be undone.",
    )));
    let apply_all = apply_all_check(tr("Apply to _all remaining items"));
    column.append(&apply_all);

    let delete = ui::button(tr("_Delete permanently"));
    delete.add_css_class("destructive-action");
    let skip = ui::button(tr("_Skip"));
    skip.add_css_class("suggested-action");
    let abort = ui::button(tr("_Abort"));
    delete.set_widget_name("ofm-trash-delete");
    skip.set_widget_name("ofm-trash-skip");
    abort.set_widget_name("ofm-trash-abort");
    for (b, permanent) in [(&delete, true), (&skip, false)] {
        let (answer, check) = (Rc::clone(answer), apply_all.clone());
        b.connect_clicked(move |_| {
            answer(Some(Answer::Confirm {
                permanent,
                apply_to_all: check.is_active(),
            }));
        });
    }
    let answer2 = Rc::clone(answer);
    abort.connect_clicked(move |_| answer2(None)); // dropping the responder aborts the job
    column.append(&ui::button_row(&[&abort, &delete, &skip]));
    let default_action: Box<dyn Fn()> = {
        let (answer, check) = (Rc::clone(answer), apply_all.clone());
        Box::new(move || {
            answer(Some(Answer::Confirm {
                permanent: false,
                apply_to_all: check.is_active(),
            }));
        })
    };
    Built {
        column,
        focus: skip.upcast(),
        default_action,
        apply_all: Some(apply_all),
        cleanup: Box::new(|| {}),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn item(size: u64, mtime_ns: i128) -> ItemInfo {
        ItemInfo {
            path: "/d/a".into(),
            kind: ItemKind::File,
            size,
            mtime_ns,
        }
    }

    #[test]
    fn hints_name_the_newer_and_larger_side() {
        let (s, e) = compare_hints(&item(10, 5), &item(5, 1));
        assert_eq!(s, ["newer", "larger"]);
        assert_eq!(e, ["older", "smaller"]);
        let (s, e) = compare_hints(&item(5, 1), &item(5, 1));
        assert_eq!(s, ["same time", "same size"]);
        assert_eq!(s, e);
    }

    #[test]
    fn errno_hints() {
        assert!(errno_hint(Some(28)).is_some_and(|h| h.contains("No space")));
        assert!(errno_hint(Some(13)).is_some());
        assert_eq!(errno_hint(Some(9999)), None);
        assert_eq!(errno_hint(None), None);
    }

    #[test]
    fn size_text_shows_exact_bytes() {
        assert_eq!(size_text(1536), "1.5 KiB (1 536 B)");
    }
}
