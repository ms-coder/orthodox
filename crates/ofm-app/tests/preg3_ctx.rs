//! T8.11 with the in-process window, driven through `AppContext::execute` (no real key events;
//! those are in `preg3_real`): config problems reach the user, Help and Preferences, every
//! `[jobs]` key takes effect on each path that starts a job (captured, never run), the context
//! menu and F3 on a folder. Needs a display and skips itself without one. One test owns the GTK
//! thread.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use adw::prelude::*;
use fileops_support::*;
use ofm_app::chrome::{contextmenu, notice};
use ofm_app::desktop::transfer;
use ofm_app::dialogs::preferences;
use ofm_app::jobs::{self, Capture};
use ofm_app::navigation::quicksearch;
use ofm_core::{cmd, commands};
use ofm_jobs::{JobSpec, RunMode};
use ofm_testkit::tree::{FileSpec, TreeSpec};

/// Unknown key `panels.sortt`; every `[jobs]` key away from its default.
const CONFIG: &str = r#"
[general]
confirm_delete = false

[panels]
sortt = "name"
filter_as_you_type = true

[jobs]
trash = false
follow_symlinks = true
fsync_after_copy = true
default_run_mode = "parallel"
"#;

fn trees() -> (TreeSpec, TreeSpec) {
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"alpha"))
        .file("b.txt", FileSpec::content(b"beta"))
        .dir(
            "sub",
            TreeSpec::new().file("in.txt", FileSpec::content(b"i")),
        );
    (left, TreeSpec::new().file("x", FileSpec::content(b"x")))
}

#[test]
fn preg3_gaps() {
    if !init() {
        return;
    }
    let (l, r) = trees();
    let fx = Fixture::with_config("io.github.mscoder.Ofm.preg3", l, r, Some(CONFIG));

    every_command_has_a_handler(&fx);
    config_warnings_are_visible(&fx);
    help_and_preferences(&fx);
    context_menu(&fx);
    typing_filters(&fx);

    let ctrl = jobs::controller(&fx.ctx).unwrap();
    let capture = ctrl.capture();
    delete_with_trash_off(&fx, &capture);
    job_defaults(&fx, &capture);
    folder_size_with_files_selected(&fx, &capture);

    reload_clears_the_warnings(&fx);
}

/// Nothing in the menus or the F-key bar is a dead end.
fn every_command_has_a_handler(fx: &Fixture) {
    let missing: Vec<&str> = commands::ALL
        .iter()
        .filter(|i| !fx.ctx.command_available(i.id))
        .map(|i| i.id.as_str())
        .collect();
    assert!(missing.is_empty(), "no handler: {missing:?}");
}

fn config_warnings_are_visible(fx: &Fixture) {
    let hint = notice::last_text(&fx.ctx).expect("a status line message");
    assert!(hint.starts_with("1 problem in config"), "{hint}");
    assert!(notice::is_shown(&fx.ctx), "the message row is showing");
    let lines = preferences::warning_lines(&fx.ctx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("panels.sortt"), "{lines:?}");
}

/// Texts of all labels under `root`.
fn label_texts(root: &gtk::Widget, out: &mut Vec<(String, bool)>) {
    if let Some(l) = root.downcast_ref::<gtk::Label>() {
        out.push((l.text().to_string(), l.is_visible()));
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        label_texts(&c, out);
        child = c.next_sibling();
    }
}

fn help_and_preferences(fx: &Fixture) {
    fx.run(cmd::APP_HELP);
    pump_for(200);
    assert_eq!(fx.modal_depth(), 1);
    let rows = || {
        let mut out = Vec::new();
        label_texts(&fx.find("ofm-help-list"), &mut out);
        out
    };
    let shows = |needle: &str| rows().iter().any(|(t, v)| *v && t.contains(needle));
    assert!(shows("panels.sortt"), "config warnings are listed in Help");
    assert!(shows("file.copy") && shows("F5"));
    let search: gtk::SearchEntry = fx.find("ofm-help-search").downcast().unwrap();
    search.set_text("file.mkdir");
    pump_until("help filtered", || !shows("file.copy"));
    assert!(shows("file.mkdir"));
    fx.run(cmd::DIALOG_CANCEL);
    pump_until("help closed", || fx.modal_depth() == 0);
    pump_for(300);

    fx.run(cmd::APP_PREFERENCES);
    pump_for(200);
    assert_eq!(fx.modal_depth(), 1);
    let problems: gtk::Label = fx.find("ofm-prefs-problems").downcast().unwrap();
    assert!(problems.text().contains("panels.sortt"));
    let path: gtk::Label = fx.find("ofm-prefs-path-config").downcast().unwrap();
    assert!(path.text().ends_with("config.toml"), "{}", path.text());
    for id in ["ofm-prefs-edit", "ofm-prefs-folder", "ofm-prefs-reload"] {
        assert!(fx.button(id).is_sensitive(), "{id}");
    }
    assert!(fx.button("ofm-prefs-edit").has_focus(), "keyboard focus");
    fx.run(cmd::DIALOG_CANCEL);
    pump_until("preferences closed", || fx.modal_depth() == 0);
    pump_for(300);
}

fn context_menu(fx: &Fixture) {
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::PANEL_CONTEXT_MENU);
    pump_for(200);
    let menu = contextmenu::current(&fx.ctx).expect("menu open");
    let shown = menu.shown_commands();
    for c in [
        cmd::FILE_VIEW,
        cmd::FILE_EDIT,
        cmd::FILE_COPY,
        cmd::FILE_DELETE,
        cmd::FILE_PROPERTIES,
    ] {
        assert!(shown.contains(&c), "{c} missing from {shown:?}");
    }
    assert_eq!(menu.accel_of(cmd::FILE_COPY).as_deref(), Some("F5"));
    // Opening again replaces the open menu; closing it by code works too.
    fx.run(cmd::PANEL_CONTEXT_MENU);
    pump_for(200);
    assert!(contextmenu::is_open(&fx.ctx));
    contextmenu::close(&fx.ctx);
    pump_for(200);
    assert!(!contextmenu::is_open(&fx.ctx));
    // An entry runs its command.
    fx.run(cmd::PANEL_CONTEXT_MENU);
    pump_for(200);
    let menu = contextmenu::current(&fx.ctx).expect("menu open");
    assert!(menu.activate(cmd::FILE_COPY));
    pump_until("copy dialog", || fx.modal_depth() == 1);
    assert!(!contextmenu::is_open(&fx.ctx));
    fx.run(cmd::DIALOG_CANCEL);
    pump_until("copy dialog closed", || fx.modal_depth() == 0);
    pump_for(300);
}

/// `[panels] filter_as_you_type`: typing narrows the list instead of jumping.
fn typing_filters(fx: &Fixture) {
    let ctrl = fx.ctx.active_controller();
    let all = ctrl.model().visible_len();
    quicksearch::start_with(&fx.ctx, 'b');
    pump_until("filtered", || ctrl.model().visible_len() < all);
    quicksearch::active_bar(&fx.ctx)
        .expect("search bar")
        .close(false);
    ctrl.mutate_view(|m| m.set_filter(None));
    pump_for(100);
    assert_eq!(ctrl.model().visible_len(), all);
}

fn last_job(capture: &Capture) -> (JobSpec, RunMode) {
    let s = capture.submitted.borrow();
    let (_, spec, mode) = s.last().expect("a job was submitted").clone();
    (spec, mode)
}

fn submitted(capture: &Capture) -> usize {
    capture.submitted.borrow().len()
}

/// `[jobs] trash = false`: F8 asks before deleting permanently, even with
/// `confirm_delete = false`, and says what it does.
fn delete_with_trash_off(fx: &Fixture, capture: &Capture) {
    fx.cursor_to(0, "a.txt");
    let before = submitted(capture);
    fx.run(cmd::FILE_DELETE);
    pump_for(200);
    assert_eq!(fx.modal_depth(), 1, "a permanent delete is confirmed");
    assert_eq!(submitted(capture), before);
    assert!(fx.check("ofm-delete-permanent").is_active());
    let heading: gtk::Label = fx.find("ofm-delete-heading").downcast().unwrap();
    assert!(!heading.text().contains("Trash"), "{}", heading.text());
    let run = fx.button("ofm-delete-run").label().unwrap();
    assert!(!run.contains("Trash"), "{run}");
    fx.run(cmd::DIALOG_OK);
    pump_until("delete submitted", || submitted(capture) == before + 1);
    match last_job(capture).0 {
        JobSpec::Delete { to_trash, .. } => assert!(!to_trash),
        other => panic!("{other:?}"),
    }
    pump_for(300);

    // With the Trash on, `confirm_delete = false` trashes at once.
    fx.ctx.config.borrow_mut().jobs.trash = true;
    fx.run(cmd::FILE_DELETE);
    assert_eq!(fx.modal_depth(), 0);
    match last_job(capture).0 {
        JobSpec::Delete { to_trash, .. } => assert!(to_trash),
        other => panic!("{other:?}"),
    }
}

/// follow_symlinks / fsync_after_copy / default_run_mode: the copy dialog starts with them and
/// paste / drag and drop (no dialog) use them.
fn job_defaults(fx: &Fixture, capture: &Capture) {
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::FILE_COPY);
    pump_for(200);
    for id in [
        "ofm-transfer-follow",
        "ofm-transfer-sync",
        "ofm-transfer-parallel",
    ] {
        assert!(fx.check(id).is_active(), "{id}");
    }
    fx.run(cmd::DIALOG_CANCEL);
    pump_until("copy dialog closed", || fx.modal_depth() == 0);
    pump_for(300);

    let source = [fx.left.path().join("a.txt")];
    assert!(transfer::submit(&fx.ctx, &source, fx.right.path(), false));
    let (spec, mode) = last_job(capture);
    assert_eq!(mode, RunMode::Parallel);
    match spec {
        JobSpec::Copy { opts, .. } => assert!(opts.follow_symlinks && opts.sync),
        other => panic!("{other:?}"),
    }

    {
        let mut cfg = fx.ctx.config.borrow_mut();
        cfg.jobs.follow_symlinks = false;
        cfg.jobs.fsync_after_copy = false;
        cfg.jobs.default_run_mode = "queued".into();
    }
    assert!(transfer::submit(&fx.ctx, &source, fx.right.path(), false));
    let (spec, mode) = last_job(capture);
    assert_eq!(mode, RunMode::Queued);
    match spec {
        JobSpec::Copy { opts, .. } => assert!(!opts.follow_symlinks && !opts.sync),
        other => panic!("{other:?}"),
    }
}

fn folder_size_with_files_selected(fx: &Fixture, capture: &Capture) {
    let ctrl = fx.ctx.active_controller();
    fx.cursor_to(0, "b.txt");
    ctrl.mutate(|m| {
        let i = m.cursor().unwrap();
        m.toggle_select(i);
    });
    fx.cursor_to(0, "sub");
    let before = submitted(capture);
    fx.run(cmd::FILE_VIEW);
    assert_eq!(submitted(capture), before + 1);
    match last_job(capture).0 {
        JobSpec::CalcSize { paths } => assert_eq!(paths, [fx.left.path().join("sub")]),
        other => panic!("{other:?}"),
    }
}

/// `app.reload_config` replaces the stored problems.
fn reload_clears_the_warnings(fx: &Fixture) {
    std::fs::write(fx.ctx.paths.config_file(), "[panels]\n").unwrap();
    fx.run(cmd::APP_RELOAD_CONFIG);
    pump_until("warnings gone", || {
        preferences::warning_lines(&fx.ctx).is_empty()
    });
}
