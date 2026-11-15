//! Copy/move dialog (T3.3 owns this file; F13, F15). Editable target (default: the other panel's
//! directory), rename mask, queue/parallel switch, options (follow symlinks, sync). Single item
//! copied into its own directory asks for a new name (`rename_mask` literal). Submits
//! `JobSpec::Copy`/`Move` through `jobs::controller(ctx)`. "Rename in place" (`file.rename`,
//! Shift+F6) is a `Move` into the same directory with a literal mask: same dialog, name field only.
//!
//! The pure validation lives in [`build_transfer`] (unit-tested); the dialog only collects text.
//! Keys: Enter = run (queued unless "run in parallel" is ticked), F2 = add to the queue, Esc =
//! cancel. Widget names (`ofm-transfer-*`) exist for the smoke tests.

use std::ffi::OsString;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use ofm_core::Location;
use ofm_core::mask::RenameMask;
use ofm_jobs::{ConflictAnswer, CopyOpts, JobSpec, RunMode};

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::actions::{self, Action};
use crate::jobs::targets::{Targets, normalize, resolve_path};
use crate::jobs::{self, SubmitOpts, ui};
use crate::navigation::modal;

/// Which transfer the dialog performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// F5.
    Copy,
    /// F6.
    Move,
    /// Shift+F6 (name field only).
    Rename,
}

/// Everything the user can decide in the dialog.
#[derive(Debug, Clone)]
pub struct TransferInput<'a> {
    /// Copy, move or rename.
    pub kind: TransferKind,
    /// Sources (see `Targets::sources`).
    pub sources: Vec<Location>,
    /// Their names, same order (for the same-name check and masks).
    pub names: Vec<OsString>,
    /// Directory the sources live in, when it is a plain local directory.
    pub src_dir: Option<&'a Path>,
    /// Home directory for `~`.
    pub home: &'a Path,
    /// Text of the destination field (ignored for rename).
    pub dest_text: &'a str,
    /// Text of the mask / new-name field.
    pub mask_text: &'a str,
    /// Copy what symlinks point to.
    pub follow_symlinks: bool,
    /// fsync everything.
    pub sync: bool,
    /// `[general] confirm_overwrite = false`: replace without asking.
    pub overwrite_silently: bool,
}

/// Validate the dialog's text and build the job. `Err` is a message for the dialog.
pub fn build_transfer(input: &TransferInput<'_>) -> Result<JobSpec, String> {
    let rename = input.kind == TransferKind::Rename;
    let dest_dir = if rename {
        input
            .src_dir
            .map(normalize)
            .ok_or_else(|| tr("Items in archives and lists cannot be renamed.").to_owned())?
    } else {
        resolve_path(
            input.dest_text,
            input.src_dir.unwrap_or(Path::new("/")),
            input.home,
        )
        .ok_or_else(|| tr("Enter a destination folder.").to_owned())?
    };

    let mask_text = input.mask_text.trim();
    if mask_text.contains('/') {
        return Err(tr("The name must not contain '/'.").to_owned());
    }
    let single = input.names.len() == 1;
    let keep_names = mask_text == "*"
        || (mask_text.is_empty() && !rename && !single)
        || (single
            && input
                .names
                .first()
                .is_some_and(|n| n.to_str() == Some(mask_text)));
    if mask_text.is_empty() && (rename || single) {
        return Err(tr("Enter a name.").to_owned());
    }
    let mask = if keep_names {
        None
    } else {
        Some(RenameMask::parse(mask_text).map_err(|e| e.to_string())?)
    };
    if rename && mask.is_none() {
        return Err(tr("The name is unchanged.").to_owned());
    }
    // Same folder and same resulting name = the item would be copied onto itself.
    if input.src_dir.map(normalize).as_deref() == Some(dest_dir.as_path()) {
        let clash = input
            .names
            .iter()
            .any(|n| mask.as_ref().is_none_or(|m| &m.apply(n) == n));
        if clash {
            return Err(tr(
                "Source and destination are the same. Enter a different name or folder.",
            )
            .to_owned());
        }
    }
    let opts = CopyOpts {
        rename_mask: mask.map(|m| m.target().to_owned()),
        follow_symlinks: input.follow_symlinks,
        sync: input.sync,
        on_conflict: input
            .overwrite_silently
            .then_some(ConflictAnswer::Overwrite),
    };
    let sources = input.sources.clone();
    Ok(match input.kind {
        TransferKind::Copy => JobSpec::Copy {
            sources,
            dest_dir,
            opts,
        },
        TransferKind::Move | TransferKind::Rename => JobSpec::Move {
            sources,
            dest_dir,
            opts,
        },
    })
}

/// Select the file name without its extension (what a rename usually changes).
fn stem_len(name: &str) -> i32 {
    let chars = |s: &str| i32::try_from(s.chars().count()).unwrap_or(i32::MAX);
    match name.rfind('.') {
        Some(p) if p > 0 => chars(&name[..p]),
        _ => chars(name),
    }
}

/// Show the dialog for the active panel's selection (or the item under the cursor).
pub fn present(ctx: &Rc<AppContext>, kind: TransferKind) {
    let Some(targets) = Targets::from_active(ctx) else {
        return;
    };
    if kind == TransferKind::Rename && targets.local_dir().is_none() {
        ui::message(
            ctx,
            tr("Rename"),
            tr("Items in archives and lists cannot be renamed."),
        );
        return;
    }
    let Some(controller) = jobs::controller(ctx) else {
        return;
    };

    let (title, verb) = match kind {
        TransferKind::Copy => (tr("Copy"), tr("_Copy")),
        TransferKind::Move => (tr("Move"), tr("_Move")),
        TransferKind::Rename => (tr("Rename"), tr("_Rename")),
    };
    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(520);
    let column = modal::padded_column(8);

    let what = targets.describe();
    let prompt = match kind {
        TransferKind::Copy => format!("{} {what} {}", tr("Copy"), tr("to:")),
        TransferKind::Move => format!("{} {what} {}", tr("Move"), tr("to:")),
        TransferKind::Rename => format!("{} {what} {}", tr("Rename"), tr("to:")),
    };

    // Destination (not for rename).
    let dest = gtk::Entry::new();
    dest.set_widget_name("ofm-transfer-dest");
    dest.update_property(&[gtk::accessible::Property::Label(tr("Destination folder"))]);
    dest.set_hexpand(true);
    if kind != TransferKind::Rename {
        column.append(&ui::label(&prompt));
        let other = ctx.other_controller().location();
        dest.set_text(
            &other
                .local_path()
                .map_or(String::new(), |p| p.to_string_lossy().into_owned()),
        );
        column.append(&dest);
    }

    // Name / mask.
    let mask = gtk::Entry::new();
    mask.set_widget_name("ofm-transfer-mask");
    mask.update_property(&[gtk::accessible::Property::Label(tr(
        "New name or rename mask",
    ))]);
    let single_name = match targets.entries.as_slice() {
        [one] => Some(one.name.to_string_lossy().into_owned()),
        _ => None,
    };
    mask.set_text(single_name.as_deref().unwrap_or("*"));
    let mask_label = match (kind, single_name.is_some()) {
        (TransferKind::Rename, _) => prompt.clone(),
        (_, true) => {
            tr("Name in the destination (a different name copies under that name):").to_owned()
        }
        (_, false) => tr("Rename mask (for example *.bak; * keeps the names):").to_owned(),
    };
    column.append(&ui::label(&mask_label));
    column.append(&mask);

    // Options.
    let follow = gtk::CheckButton::with_mnemonic(tr("Follow symbolic _links"));
    follow.set_widget_name("ofm-transfer-follow");
    let sync = gtk::CheckButton::with_mnemonic(tr("_Sync to disk (fsync)"));
    sync.set_widget_name("ofm-transfer-sync");
    let parallel =
        gtk::CheckButton::with_mnemonic(tr("Run in _parallel (do not wait in the queue)"));
    parallel.set_widget_name("ofm-transfer-parallel");
    if kind != TransferKind::Rename {
        column.append(&follow);
        column.append(&sync);
        column.append(&parallel);
    }
    let error = ui::error_label();
    error.set_widget_name("ofm-transfer-error");
    column.append(&error);

    let cancel = ui::button(tr("Cancel"));
    let queue = ui::button(tr("_Queue"));
    queue.set_tooltip_text(Some(tr("Add to the job queue (F2)")));
    let run = ui::button(verb);
    run.add_css_class("suggested-action");
    run.set_widget_name("ofm-transfer-run");
    if kind == TransferKind::Rename {
        column.append(&ui::button_row(&[&cancel, &run]));
    } else {
        column.append(&ui::button_row(&[&cancel, &queue, &run]));
    }
    dialog.set_child(Some(&modal::with_header(&column)));
    let first = if kind == TransferKind::Rename {
        mask.clone()
    } else {
        dest.clone()
    };
    dialog.set_focus(Some(&first));
    if kind == TransferKind::Rename {
        mask.select_region(0, stem_len(single_name.as_deref().unwrap_or("")));
    }

    // Submit: validates, then runs the job in the chosen mode; returns true when it was accepted.
    let submit: Rc<dyn Fn(bool) -> bool> = {
        let (ctx, targets, controller) = (Rc::clone(ctx), targets.clone(), Rc::clone(&controller));
        let (dest, mask, follow, sync, parallel, error) = (
            dest.clone(),
            mask.clone(),
            follow.clone(),
            sync.clone(),
            parallel.clone(),
            error.clone(),
        );
        Rc::new(move |force_queue: bool| {
            let home = ui::home();
            let input = TransferInput {
                kind,
                sources: targets.sources(),
                names: targets.names(),
                src_dir: targets.local_dir(),
                home: &home,
                dest_text: &dest.text(),
                mask_text: &mask.text(),
                follow_symlinks: follow.is_active(),
                sync: sync.is_active(),
                overwrite_silently: !ctx.config.borrow().general.confirm_overwrite,
            };
            match build_transfer(&input) {
                Ok(spec) => {
                    let mode = if parallel.is_active() && !force_queue {
                        RunMode::Parallel
                    } else {
                        RunMode::Queued
                    };
                    controller.submit_with(
                        &ctx,
                        spec,
                        mode,
                        SubmitOpts {
                            origin: Some(Rc::downgrade(&targets.controller)),
                            on_finished: None,
                        },
                    );
                    true
                }
                Err(msg) => {
                    ui::set_error(&error, Some(&msg));
                    false
                }
            }
        })
    };

    let weak = dialog.downgrade();
    {
        let (submit, weak) = (Rc::clone(&submit), weak.clone());
        run.connect_clicked(move |_| {
            if submit(false)
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
        });
    }
    {
        let (submit, weak) = (Rc::clone(&submit), weak.clone());
        queue.connect_clicked(move |_| {
            if submit(true)
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
        });
    }
    {
        let weak = weak.clone();
        cancel.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    // Typing clears a stale validation message.
    for e in [&dest, &mask] {
        let error = error.clone();
        e.connect_changed(move |_| ui::set_error(&error, None));
    }

    let ctx_ok = Rc::clone(ctx);
    let on_ok = {
        let submit = Rc::clone(&submit);
        move || {
            if ui::activate_focused_button(&ctx_ok) {
                return false;
            }
            submit(false)
        }
    };
    if modal::present(ctx, &dialog, on_ok) {
        actions::bind(ctx, &dialog, move |a| {
            if a != Action::Queue {
                return false;
            }
            if submit(true)
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
            true
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn input<'a>(
        kind: TransferKind,
        names: &[&str],
        src: &'a Path,
        home: &'a Path,
        dest: &'a str,
        mask: &'a str,
    ) -> TransferInput<'a> {
        TransferInput {
            kind,
            sources: names.iter().map(|n| Location::Local(src.join(n))).collect(),
            names: names.iter().map(OsString::from).collect(),
            src_dir: Some(src),
            home,
            dest_text: dest,
            mask_text: mask,
            follow_symlinks: false,
            sync: false,
            overwrite_silently: false,
        }
    }

    fn opts(spec: &JobSpec) -> (&Path, Option<&str>) {
        match spec {
            JobSpec::Copy { dest_dir, opts, .. } | JobSpec::Move { dest_dir, opts, .. } => {
                (dest_dir.as_path(), opts.rename_mask.as_deref())
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn copy_to_other_dir_keeps_names() {
        let (src, home) = (Path::new("/a"), Path::new("/h"));
        let i = input(
            TransferKind::Copy,
            &["x.txt", "y.txt"],
            src,
            home,
            "/b",
            "*",
        );
        let spec = build_transfer(&i).expect("valid");
        assert!(matches!(spec, JobSpec::Copy { .. }));
        assert_eq!(opts(&spec), (Path::new("/b"), None));
        // A single item whose name field was left alone also keeps its name.
        let i = input(TransferKind::Move, &["x.txt"], src, home, "~/dst", "x.txt");
        let spec = build_transfer(&i).expect("valid");
        assert!(matches!(spec, JobSpec::Move { .. }));
        assert_eq!(opts(&spec), (Path::new("/h/dst"), None));
    }

    #[test]
    fn masks_and_new_names() {
        let (src, home) = (Path::new("/a"), Path::new("/h"));
        let i = input(
            TransferKind::Copy,
            &["x.txt", "y.txt"],
            src,
            home,
            "/b",
            "*.bak",
        );
        assert_eq!(opts(&build_transfer(&i).expect("valid")).1, Some("*.bak"));
        let i = input(TransferKind::Copy, &["x.txt"], src, home, "/b", "other.txt");
        assert_eq!(
            opts(&build_transfer(&i).expect("valid")).1,
            Some("other.txt")
        );
        let i = input(TransferKind::Copy, &["x.txt"], src, home, "/b", "a/b");
        assert!(build_transfer(&i).is_err());
        let i = input(TransferKind::Copy, &["x.txt"], src, home, "/b", "");
        assert!(build_transfer(&i).is_err());
        let i = input(TransferKind::Copy, &["x.txt"], src, home, "/b", "{x}");
        assert!(build_transfer(&i).is_err(), "braces are not allowed");
    }

    #[test]
    fn same_directory_needs_a_new_name() {
        let (src, home) = (Path::new("/a"), Path::new("/h"));
        // Copy into the own folder under the same name: refused.
        let i = input(TransferKind::Copy, &["x.txt"], src, home, "/a/", "x.txt");
        assert!(build_transfer(&i).is_err());
        let i = input(TransferKind::Copy, &["x.txt", "y"], src, home, ".", "*");
        assert!(build_transfer(&i).is_err());
        // With a new name it is fine, and a mask that changes every name too.
        let i = input(
            TransferKind::Copy,
            &["x.txt"],
            src,
            home,
            "/a",
            "x copy.txt",
        );
        let spec = build_transfer(&i).expect("valid");
        assert_eq!(opts(&spec), (Path::new("/a"), Some("x copy.txt")));
        let i = input(
            TransferKind::Copy,
            &["x.txt", "y.txt"],
            src,
            home,
            "/a",
            "*.bak",
        );
        assert!(build_transfer(&i).is_ok());
    }

    #[test]
    fn rename_is_a_move_into_the_same_directory() {
        let (src, home) = (Path::new("/a"), Path::new("/h"));
        let i = input(TransferKind::Rename, &["x.txt"], src, home, "", "z.txt");
        let spec = build_transfer(&i).expect("valid");
        assert!(matches!(spec, JobSpec::Move { .. }));
        assert_eq!(opts(&spec), (Path::new("/a"), Some("z.txt")));
        let i = input(TransferKind::Rename, &["x.txt"], src, home, "", "x.txt");
        assert!(build_transfer(&i).is_err(), "unchanged name");
        let i = input(TransferKind::Rename, &["x.txt"], src, home, "", "");
        assert!(build_transfer(&i).is_err());
        let mut i = input(TransferKind::Rename, &["x.txt"], src, home, "", "y");
        i.src_dir = None;
        assert!(build_transfer(&i).is_err(), "no local folder");
    }

    #[test]
    fn options_flow_into_the_job() {
        let (src, home) = (Path::new("/a"), Path::new("/h"));
        let mut i = input(TransferKind::Copy, &["x"], src, home, "/b", "*");
        i.follow_symlinks = true;
        i.sync = true;
        i.overwrite_silently = true;
        match build_transfer(&i).expect("valid") {
            JobSpec::Copy { opts, .. } => {
                assert!(opts.follow_symlinks && opts.sync);
                assert_eq!(opts.on_conflict, Some(ConflictAnswer::Overwrite));
            }
            other => panic!("unexpected {other:?}"),
        }
        let i = input(TransferKind::Copy, &["x"], src, home, "  ", "*");
        assert!(build_transfer(&i).is_err(), "empty destination");
    }

    #[test]
    fn stem_selection_skips_the_extension() {
        assert_eq!(stem_len("report.final.pdf"), 12);
        assert_eq!(stem_len(".bashrc"), 7);
        assert_eq!(stem_len("noext"), 5);
    }
}
