//! T3.3: the file-operation dialogs (copy/move/rename, delete, mkdir, link, properties) driven
//! through `AppContext::execute`, with the engine replaced by `JobsController::capture` (no worker
//! threads: the submitted `JobSpec`s are inspected). Needs a display (Xvfb in `check.sh`) and
//! skips itself without one. `OFM_SHOT_DIR=dir` additionally writes screenshots (far theme).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use std::path::PathBuf;

use fileops_support::*;
use gtk::prelude::*;
use ofm_app::dispatch::CommandRegistry;
use ofm_app::jobs::{self, Capture};
use ofm_config::{ConfigPaths, Keymap, Preset};
use ofm_core::{CommandId, KeyChord, KeyContext, Location, cmd};
use ofm_jobs::{JobSpec, LinkKind, ModeChange, RunMode};
use ofm_testkit::tree::{FileSpec, TreeSpec};

// ---- commands and keys (headless) -------------------------------------------------------------------

const MAIN_KEYS: &[(&str, CommandId)] = &[
    ("F5", cmd::FILE_COPY),
    ("F6", cmd::FILE_MOVE),
    ("Shift+F6", cmd::FILE_RENAME),
    ("F7", cmd::FILE_MKDIR),
    ("F8", cmd::FILE_DELETE),
    ("Shift+Del", cmd::FILE_DELETE_PERMANENT),
    ("Alt+F6", cmd::FILE_LINK),
    ("Alt+Enter", cmd::FILE_PROPERTIES),
    ("Alt+Shift+Enter", cmd::FILE_CALC_SIZE),
    ("Ctrl+J", cmd::APP_JOBS),
];

const DIALOG_KEYS: &[(&str, CommandId)] = &[
    ("Enter", cmd::DIALOG_OK),
    ("Esc", cmd::DIALOG_CANCEL),
    ("F2", cmd::DIALOG_QUEUE),
    ("Ctrl+A", cmd::DIALOG_APPLY_ALL),
    ("Space", cmd::JOBS_PAUSE_RESUME),
    ("Del", cmd::JOBS_CANCEL),
    ("Ctrl+L", cmd::JOBS_CLEAR_FINISHED),
];

const ALL_COMMANDS: &[CommandId] = &[
    cmd::FILE_COPY,
    cmd::FILE_MOVE,
    cmd::FILE_RENAME,
    cmd::FILE_MKDIR,
    cmd::FILE_DELETE,
    cmd::FILE_DELETE_PERMANENT,
    cmd::FILE_LINK,
    cmd::FILE_PROPERTIES,
    cmd::FILE_CALC_SIZE,
    cmd::APP_JOBS,
    cmd::JOBS_PAUSE_RESUME,
    cmd::JOBS_CANCEL,
    cmd::JOBS_CLEAR_FINISHED,
    cmd::DIALOG_QUEUE,
    cmd::DIALOG_APPLY_ALL,
];

#[test]
fn file_operation_commands_have_handlers_and_keys() {
    let mut reg = CommandRegistry::new();
    ofm_app::hooks::install_commands(&mut reg);
    assert!(reg.duplicates().is_empty(), "{:?}", reg.duplicates());
    for c in ALL_COMMANDS {
        assert!(reg.has(*c), "no handler for {c}");
    }
    let tmp = TreeSpec::new().build_temp().unwrap();
    let paths = ConfigPaths::with_dirs(tmp.path().join("c"), tmp.path().join("s"));
    for preset in [Preset::Far, Preset::Tc, Preset::Mc] {
        let km = Keymap::load(&paths, preset, None).0;
        for (chord, command) in MAIN_KEYS {
            let chord: KeyChord = chord.parse().unwrap();
            // Mc/Tc may bind a few of these differently; the commands must at least be reachable.
            let bound = km.resolve_main(chord, false);
            if preset == Preset::Far {
                assert_eq!(bound, Some(*command), "{preset:?}: {chord}");
            }
        }
        for (chord, command) in DIALOG_KEYS {
            let chord: KeyChord = chord.parse().unwrap();
            assert_eq!(
                km.resolve_in(KeyContext::Dialog, chord),
                Some(*command),
                "{preset:?}: {chord}"
            );
        }
    }
}

// ---- UI scenario -------------------------------------------------------------------------------------

fn spec_of(cap: &Capture, index: usize) -> (JobSpec, RunMode) {
    let s = cap.submitted.borrow();
    let (_, spec, mode) = s[index].clone();
    (spec, mode)
}

fn local(p: &std::path::Path) -> Location {
    Location::Local(p.to_owned())
}

#[test]
fn dialogs_scenario() {
    if !init() {
        return;
    }
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"aaa").mode(0o644))
        .file("b.txt", FileSpec::content(b"bbb").mode(0o644))
        .file("run.sh", FileSpec::content(b"#!/bin/sh\n").mode(0o755))
        .dir(
            "sub",
            TreeSpec::new().file("inner.txt", FileSpec::content(b"inner")),
        );
    let right = TreeSpec::new().file("r.txt", FileSpec::content(b"r"));
    let fx = Fixture::new(
        "io.github.mscoder.OfmFileopsDialogs",
        left,
        right,
        theme_from_env().as_deref(),
    );
    let ctx = &fx.ctx;
    let ctrl = jobs::controller(ctx).unwrap();
    let cap = ctrl.capture();
    let (lp, rp) = (fx.left.path().to_owned(), fx.right.path().to_owned());

    // ---- copy: defaults, mask, queue --------------------------------------------------------------
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::FILE_COPY);
    assert_eq!(fx.modal_depth(), 1);
    assert!(ctx.modal_open());
    assert_eq!(
        fx.entry("ofm-transfer-dest").text().as_str(),
        rp.to_string_lossy()
    );
    assert_eq!(fx.entry("ofm-transfer-mask").text(), "a.txt");
    // Real keys reach the keymap inside a dialog only through its own dispatcher controller
    // (libadwaita does not propagate key events from a dialog to the window).
    let mut w = fx.find("ofm-transfer-mask").parent();
    let mut dialog_has_dispatcher = false;
    while let Some(p) = w {
        if p.parent().is_some() && ofm_app::dispatch::has_dispatch_controller(&p) {
            dialog_has_dispatcher = true;
        }
        w = p.parent();
    }
    assert!(dialog_has_dispatcher, "the dialog has no key dispatcher");
    fx.shot("copy-dialog");
    fx.run(cmd::DIALOG_OK);
    assert_eq!(fx.modal_depth(), 0, "Enter accepts and closes");
    assert!(!ctx.modal_open());
    assert_eq!(cap.submitted.borrow().len(), 1);
    let (spec, mode) = spec_of(&cap, 0);
    match spec {
        JobSpec::Copy {
            sources,
            dest_dir,
            opts,
        } => {
            assert_eq!(sources, vec![local(&lp.join("a.txt"))]);
            assert_eq!(dest_dir, rp);
            assert_eq!(opts.rename_mask, None);
            assert!(!opts.follow_symlinks && !opts.sync);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(mode, RunMode::Queued, "queued by default");

    // Selection of two files + mask + parallel; F2 (dialog.queue) overrides parallel.
    for n in ["a.txt", "b.txt"] {
        fx.cursor_to(0, n);
        let c = ctx.active_controller();
        c.mutate(|m| {
            let i = m.cursor().unwrap();
            m.toggle_select(i);
        });
    }
    fx.run(cmd::FILE_COPY);
    assert_eq!(fx.entry("ofm-transfer-mask").text(), "*");
    fx.entry("ofm-transfer-mask").set_text("*.bak");
    fx.check("ofm-transfer-parallel").set_active(true);
    fx.check("ofm-transfer-sync").set_active(true);
    fx.shot("copy-dialog-mask");
    fx.run(cmd::DIALOG_OK);
    let (spec, mode) = spec_of(&cap, 1);
    assert_eq!(mode, RunMode::Parallel);
    match spec {
        JobSpec::Copy { sources, opts, .. } => {
            assert_eq!(sources.len(), 2);
            assert_eq!(opts.rename_mask.as_deref(), Some("*.bak"));
            assert!(opts.sync);
        }
        other => panic!("{other:?}"),
    }
    fx.run(cmd::FILE_COPY);
    fx.check("ofm-transfer-parallel").set_active(true);
    fx.run(cmd::DIALOG_QUEUE);
    assert_eq!(fx.modal_depth(), 0);
    assert_eq!(spec_of(&cap, 2).1, RunMode::Queued, "F2 queues");

    // Same directory with the same name: refused, the dialog stays and says why.
    fx.run(cmd::FILE_COPY);
    fx.entry("ofm-transfer-dest")
        .set_text(&lp.to_string_lossy());
    fx.run(cmd::DIALOG_OK);
    assert_eq!(fx.modal_depth(), 1, "invalid input keeps the dialog open");
    assert!(fx.find("ofm-transfer-error").is_visible());
    fx.shot("copy-dialog-error");
    assert_eq!(cap.submitted.borrow().len(), 3);
    // A new name makes it valid.
    fx.entry("ofm-transfer-mask").set_text("copy of x");
    assert!(
        !fx.find("ofm-transfer-error").is_visible(),
        "typing clears the message"
    );
    fx.run(cmd::DIALOG_CANCEL);
    assert_eq!(fx.modal_depth(), 0);
    assert_eq!(cap.submitted.borrow().len(), 3, "Esc submits nothing");

    // ---- move and rename ---------------------------------------------------------------------------
    ctx.active_controller().mutate(|m| m.select_all(false));
    fx.cursor_to(0, "run.sh");
    fx.run(cmd::FILE_MOVE);
    fx.run(cmd::DIALOG_OK);
    assert!(matches!(spec_of(&cap, 3).0, JobSpec::Move { .. }));
    fx.run(cmd::FILE_RENAME);
    fx.entry("ofm-transfer-mask").set_text("run2.sh");
    fx.shot("rename-dialog");
    fx.run(cmd::DIALOG_OK);
    match spec_of(&cap, 4).0 {
        JobSpec::Move { dest_dir, opts, .. } => {
            assert_eq!(dest_dir, lp);
            assert_eq!(opts.rename_mask.as_deref(), Some("run2.sh"));
        }
        other => panic!("{other:?}"),
    }
    // Every submit shows up in the job table.
    assert_eq!(ctrl.jobs().len(), 5);

    // ---- delete --------------------------------------------------------------------------------------
    fx.cursor_to(0, "b.txt");
    fx.run(cmd::FILE_DELETE);
    assert_eq!(fx.modal_depth(), 1);
    assert!(!fx.check("ofm-delete-permanent").is_active());
    fx.shot("delete-dialog");
    fx.run(cmd::DIALOG_OK);
    match spec_of(&cap, 5).0 {
        JobSpec::Delete { paths, to_trash } => {
            assert_eq!(paths, vec![lp.join("b.txt")]);
            assert!(to_trash, "trash by default (F14)");
        }
        other => panic!("{other:?}"),
    }
    fx.cursor_to(0, "sub");
    fx.run(cmd::FILE_DELETE_PERMANENT);
    assert!(fx.check("ofm-delete-permanent").is_active());
    fx.shot("delete-permanent-dialog");
    fx.run(cmd::DIALOG_OK);
    match spec_of(&cap, 6).0 {
        JobSpec::Delete { paths, to_trash } => {
            assert_eq!(paths, vec![lp.join("sub")]);
            assert!(!to_trash);
        }
        other => panic!("{other:?}"),
    }
    // Un-ticking the box in the permanent dialog switches back to the Trash.
    fx.run(cmd::FILE_DELETE_PERMANENT);
    fx.check("ofm-delete-permanent").set_active(false);
    fx.run(cmd::DIALOG_OK);
    assert!(matches!(
        spec_of(&cap, 7).0,
        JobSpec::Delete { to_trash: true, .. }
    ));
    // Cancel with Esc does nothing.
    fx.run(cmd::FILE_DELETE);
    fx.run(cmd::DIALOG_CANCEL);
    assert_eq!(cap.submitted.borrow().len(), 8);

    // ---- mkdir ---------------------------------------------------------------------------------------
    fx.run(cmd::FILE_MKDIR);
    fx.run(cmd::DIALOG_OK);
    assert_eq!(fx.modal_depth(), 1, "empty name is refused");
    assert!(fx.find("ofm-mkdir-error").is_visible());
    fx.entry("ofm-mkdir-name").set_text("new/nested");
    fx.shot("mkdir-dialog");
    fx.run(cmd::DIALOG_OK);
    assert_eq!(fx.modal_depth(), 0);
    let (spec, mode) = spec_of(&cap, 8);
    assert_eq!(mode, RunMode::Parallel, "mkdir does not wait behind copies");
    match spec {
        JobSpec::Mkdir { path } => assert_eq!(path, lp.join("new/nested")),
        other => panic!("{other:?}"),
    }

    // ---- link ----------------------------------------------------------------------------------------
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::FILE_LINK);
    assert_eq!(
        fx.entry("ofm-link-path").text().as_str(),
        rp.join("a.txt").to_string_lossy()
    );
    fx.check("ofm-link-relative").set_active(true);
    fx.shot("link-dialog");
    fx.run(cmd::DIALOG_OK);
    match spec_of(&cap, 9).0 {
        JobSpec::Link { target, link, kind } => {
            assert_eq!(kind, LinkKind::Symbolic);
            assert_eq!(link, rp.join("a.txt"));
            let up = PathBuf::from("..")
                .join(lp.file_name().unwrap())
                .join("a.txt");
            assert_eq!(target, up, "relative to the link's directory");
        }
        other => panic!("{other:?}"),
    }
    fx.cursor_to(0, "sub");
    fx.run(cmd::FILE_LINK);
    assert!(
        !fx.check("ofm-link-hard").is_sensitive(),
        "no hard links to folders"
    );
    fx.run(cmd::DIALOG_CANCEL);

    // ---- properties --------------------------------------------------------------------------------
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::FILE_PROPERTIES);
    assert_eq!(fx.entry("ofm-props-octal").text(), "0644");
    assert!(fx.check("ofm-props-bit0").is_active(), "owner read");
    assert!(!fx.check("ofm-props-bit2").is_active(), "owner execute");
    fx.shot("properties-dialog");
    fx.run(cmd::DIALOG_OK);
    assert_eq!(cap.submitted.borrow().len(), 10, "no change, no job");
    fx.run(cmd::FILE_PROPERTIES);
    fx.entry("ofm-props-octal").set_text("0600");
    assert!(
        !fx.check("ofm-props-bit3").is_active(),
        "octal updates the boxes"
    );
    fx.check("ofm-props-bit2").set_active(true); // boxes update the octal
    assert_eq!(fx.entry("ofm-props-octal").text(), "0700");
    fx.run(cmd::DIALOG_OK);
    match spec_of(&cap, 10).0 {
        JobSpec::SetAttrs {
            paths,
            mode,
            owner,
            recursive,
        } => {
            assert_eq!(paths, vec![lp.join("a.txt")]);
            assert_eq!(
                mode,
                Some(ModeChange {
                    set: 0o100,
                    clear: 0o044
                })
            );
            assert!(owner.is_none() && !recursive);
        }
        other => panic!("{other:?}"),
    }
    // A folder: its size is calculated in the background and cancelled when the dialog closes.
    fx.cursor_to(0, "sub");
    fx.run(cmd::FILE_PROPERTIES);
    let before = cap.submitted.borrow().len();
    assert!(matches!(
        spec_of(&cap, before - 1).0,
        JobSpec::CalcSize { .. }
    ));
    assert_eq!(
        fx.find("ofm-props-size")
            .downcast::<gtk::Label>()
            .unwrap()
            .text(),
        "Calculating..."
    );
    fx.shot("properties-folder");
    fx.run(cmd::DIALOG_CANCEL);
    assert!(
        cap.controls
            .borrow()
            .iter()
            .any(|c| matches!(c, jobs::Control::Cancel(_))),
        "closing the dialog cancels the size job"
    );
    // Selecting both mixed-permission files shows tri-state boxes and no octal value.
    ctx.active_controller().mutate(|m| m.select_all(false));
    for n in ["a.txt", "run.sh"] {
        fx.cursor_to(0, n);
        ctx.active_controller().mutate(|m| {
            let i = m.cursor().unwrap();
            m.toggle_select(i);
        });
    }
    fx.run(cmd::FILE_PROPERTIES);
    assert_eq!(fx.entry("ofm-props-octal").text(), "");
    assert!(
        fx.check("ofm-props-bit2").is_inconsistent(),
        "execute differs"
    );
    assert!(!fx.check("ofm-props-bit0").is_inconsistent());
    fx.shot("properties-mixed");
    fx.run(cmd::DIALOG_CANCEL);

    assert_eq!(fx.modal_depth(), 0);
    assert!(!ctx.modal_open());
}
