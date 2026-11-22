//! T8.7 UI polish with the in-process window (no real key events): filter indicator, "Pausing..."
//! / "Cancelling..." job rows, quick-view header and status, folder-size cache. Needs a display
//! and skips itself without one. One test owns the GTK thread.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use std::ffi::OsString;

use fileops_support::*;
use ofm_app::jobs::dirsize::DirSizeCache;
use ofm_app::jobs::fmt::state_text;
use ofm_app::jobs::{self, Requested};
use ofm_core::cmd;
use ofm_jobs::{JobEvent, JobId, JobState, Outcome, RunMode};
use ofm_testkit::tree::{FileSpec, TreeSpec};

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
fn polish_behaviours() {
    if !init() {
        return;
    }
    let (l, r) = trees();
    let fx = Fixture::new("io.github.mscoder.Ofm.polish", l, r, Some("far-blue"));

    filter_indicator(&fx);
    pending_job_requests(&fx);
    quick_view_header(&fx);
    folder_size_cache(&fx);
}

/// A filter that stays after the bar closed is named in the status line until it is cleared.
fn filter_indicator(fx: &Fixture) {
    let ctrl = fx.ctx.host(0).active();
    let summary = || fx.win.left.summary_label.text().to_string();
    assert!(!summary().contains("filter:"), "{}", summary());
    ctrl.mutate_view(|m| m.set_filter(Some("txt")));
    pump_for(100);
    assert!(summary().contains("filter: txt"), "{}", summary());
    ctrl.mutate_view(|m| m.set_filter(None));
    pump_for(100);
    assert!(!summary().contains("filter:"), "{}", summary());
}

/// Pause and cancel show "Pausing..." / "Cancelling..." at once, not when the worker stops.
fn pending_job_requests(fx: &Fixture) {
    let ctrl = jobs::controller(&fx.ctx).unwrap();
    let event = |ev| ctrl.handle_event(&fx.ctx, ev);
    event(JobEvent::Queued {
        id: JobId(7),
        title: "copy".into(),
        mode: RunMode::Parallel,
    });
    event(JobEvent::Started { id: JobId(7) });
    let job = || ctrl.job(JobId(7)).unwrap();
    assert_eq!(job().requested, None);

    ctrl.pause(JobId(7));
    assert_eq!(job().requested, Some(Requested::Pausing));
    assert_eq!(
        state_text(job().state, job().asking, job().requested),
        "Pausing\u{2026}"
    );
    // The worker reports the pause: the request is fulfilled.
    event(JobEvent::StateChanged {
        id: JobId(7),
        state: JobState::Paused,
    });
    assert_eq!(job().requested, None);
    assert_eq!(state_text(job().state, false, job().requested), "Paused");

    ctrl.cancel(JobId(7));
    assert_eq!(job().requested, Some(Requested::Cancelling));
    assert_eq!(
        state_text(job().state, false, job().requested),
        "Cancelling\u{2026}"
    );
    // Look at it (OFM_SHOT_DIR=dir): the open jobs view shows the row as cancelling.
    fx.run(cmd::APP_JOBS);
    pump_for(1500);
    fx.shot("polish-jobs-cancelling");
    fx.run(cmd::APP_JOBS);
    event(JobEvent::Finished {
        id: JobId(7),
        outcome: Outcome::Cancelled(Default::default()),
    });
    assert_eq!(job().requested, None);
    assert_eq!(job().state, JobState::Finished);
}

/// The preview side says "Quick view" and describes the previewed item; leaving restores both.
fn quick_view_header(fx: &Fixture) {
    let right = &fx.win.right;
    let before = right.status_label.text().to_string();
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::PANEL_TOGGLE_QUICKVIEW);
    pump_for(500);
    let during = right.status_label.text().to_string();
    assert!(
        during.starts_with("a.txt"),
        "status while previewing: {during:?}"
    );
    fx.cursor_to(0, "b.txt");
    pump_for(500);
    assert!(
        right.status_label.text().starts_with("b.txt"),
        "the status line follows the cursor: {:?}",
        right.status_label.text()
    );
    fx.run(cmd::PANEL_TOGGLE_QUICKVIEW);
    pump_for(300);
    assert_eq!(right.status_label.text().to_string(), before);
    assert_eq!(
        right.path_label.text().to_string(),
        fx.ctx.host(1).active().location().display_lossy()
    );
}

/// A calculated folder size survives the watcher re-stat that brings the file system's size back.
fn folder_size_cache(fx: &Fixture) {
    let panel = fx.ctx.host(0).active();
    let loc = panel.location();
    let sub = {
        let m = panel.model();
        (0..m.visible_len())
            .filter_map(|i| m.visible(i))
            .find(|e| e.name == "sub")
            .cloned()
            .unwrap()
    };
    let cache = fx.ctx.ext.get::<DirSizeCache>().unwrap();
    cache.store(&panel, &loc, OsString::from("sub"), sub.mtime_ns, 123_456);
    let mut shown = sub.clone();
    shown.size = 123_456;
    panel.mutate_view(|m| m.apply_update(vec![shown], &[]));
    let size_of = || {
        let m = panel.model();
        (0..m.visible_len())
            .filter_map(|i| m.visible(i))
            .find(|e| e.name == "sub")
            .map(|e| e.size)
            .unwrap()
    };
    assert_eq!(size_of(), 123_456);

    // The watcher re-stats the entry: same mtime, the file system's own size.
    let mut restat = sub.clone();
    restat.size = 4096;
    panel.mutate_view(|m| m.apply_update(vec![restat], &[]));
    pump_for(300);
    assert_eq!(size_of(), 123_456, "the calculated size was lost");

    // The folder changed (new mtime): the stale number is not forced back.
    let mut changed = sub;
    changed.size = 8192;
    changed.mtime_ns += 5_000_000_000;
    panel.mutate_view(|m| m.apply_update(vec![changed], &[]));
    pump_for(300);
    assert_eq!(size_of(), 8192);
}
