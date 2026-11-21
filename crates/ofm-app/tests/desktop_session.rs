//! T8.4 (F33): session capture and restore inside the app (no process restart): tabs, paths,
//! cursor names, view modes, sort, hidden flag, active tab and panel survive
//! `session::snapshot` -> `session::restore_panels` into a fresh window; a restored directory that
//! no longer exists falls back to its nearest existing parent. The real restart through the binary
//! is `desktop_real.rs`. Needs a display; skips without one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use std::path::PathBuf;

use fileops_support::*;
use ofm_app::navigation::goto::navigate;
use ofm_app::panel::LoadState;
use ofm_app::window::session;
use ofm_config::session::{PanelSession, Session, TabSession};
use ofm_core::{Location, cmd};
use ofm_testkit::tree::{FileSpec, TreeSpec};

fn tree() -> (TreeSpec, TreeSpec) {
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file(".hidden", FileSpec::content(b"h"))
        .dir(
            "sub",
            TreeSpec::new()
                .file("s1", FileSpec::content(b"1"))
                .file("s2", FileSpec::content(b"22"))
                .file("s3", FileSpec::content(b"333")),
        );
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    (left, right)
}

fn settled_at(fx: &Fixture, side: usize, want: &std::path::Path) -> bool {
    let tab = fx.ctx.host(side).active();
    tab.state() == LoadState::Idle && tab.location() == Location::Local(want.to_owned())
}

#[test]
fn session_state_survives_snapshot_and_restore() {
    if !init() {
        return;
    }
    let (l, r) = tree();
    let a = Fixture::new(
        "io.github.mscoder.OfmDesktopSessionA",
        l,
        r,
        theme_from_env().as_deref(),
    );
    let sub = a.left.path().join("sub");

    // Left: into `sub`, cursor on s2, full view, sort by size reversed, hidden files shown;
    // then a second tab (a copy of this one) with brief view and cursor on s3.
    let ctrl = a.ctx.host(0).active();
    navigate(&a.ctx, &ctrl, Location::Local(sub.clone()), None);
    pump_until("sub loaded", || settled_at(&a, 0, &sub));
    a.cursor_to(0, "s2");
    a.run(cmd::PANEL_VIEW_FULL);
    a.run(cmd::PANEL_SORT_SIZE);
    a.run(cmd::PANEL_SORT_REVERSE);
    a.run(cmd::PANEL_TOGGLE_HIDDEN);
    a.run(cmd::TAB_NEW);
    pump_until("second tab loaded", || {
        a.ctx.host(0).tabs().len() == 2 && settled_at(&a, 0, &sub)
    });
    a.cursor_to(0, "s3");
    a.run(cmd::PANEL_VIEW_BRIEF);
    // Right panel active, custom view.
    a.ctx.set_active_side(1);
    a.run(cmd::PANEL_VIEW_CUSTOM);
    a.cursor_to(1, "x1");

    let before = session::snapshot(&a.ctx);
    assert_eq!(before.active_panel, 1);
    assert_eq!(before.panels.len(), 2);
    let left = &before.panels[0];
    assert_eq!(left.tabs.len(), 2);
    assert_eq!(left.active_tab, 1);
    assert_eq!(left.tabs[0].view, "full");
    assert_eq!(left.tabs[1].view, "brief");
    assert_eq!(left.tabs[0].cursor_name.as_deref(), Some("s2"));
    assert_eq!(left.tabs[1].cursor_name.as_deref(), Some("s3"));
    assert_eq!(left.tabs[0].sort, "size");
    assert!(left.tabs[0].sort_reverse);
    assert!(left.tabs[0].show_hidden);
    assert_eq!(before.panels[1].tabs[0].view, "custom");

    // The same session into a fresh window.
    let (l2, r2) = tree();
    let b = Fixture::new(
        "io.github.mscoder.OfmDesktopSessionB",
        l2,
        r2,
        theme_from_env().as_deref(),
    );
    session::restore_panels(&b.ctx, &before);
    pump_until("restored", || {
        b.ctx.host(0).tabs().len() == 2
            && b.ctx
                .host(0)
                .tabs()
                .iter()
                .all(|t| t.state() == LoadState::Idle)
            && b.ctx.host(1).active().state() == LoadState::Idle
    });
    pump_for(300);
    let after = session::snapshot(&b.ctx);
    assert_eq!(after.active_panel, 1);
    assert_eq!(after.panels, before.panels);

    // A restored directory that is gone falls back to its nearest existing ancestor.
    let c_dirs = tree();
    let c = Fixture::new(
        "io.github.mscoder.OfmDesktopSessionC",
        c_dirs.0,
        c_dirs.1,
        theme_from_env().as_deref(),
    );
    let existing: PathBuf = c.left.path().join("sub");
    let mut gone = TabSession::default();
    gone.set_path(&existing.join("deleted").join("deeper"));
    gone.set_cursor_name("whatever".as_ref());
    let mut ok = TabSession::default();
    ok.set_path(c.right.path());
    let stored = Session {
        active_panel: 9, // out of range
        panels: vec![
            PanelSession {
                active_tab: 7, // out of range
                tabs: vec![gone],
            },
            PanelSession {
                active_tab: 0,
                tabs: vec![ok],
            },
            PanelSession::default(), // a third panel is ignored
        ],
        ..Session::default()
    };
    let mut stored = stored;
    session::sanitize(&mut stored);
    session::restore_panels(&c.ctx, &stored);
    pump_until("fell back to the existing parent", || {
        settled_at(&c, 0, &existing)
    });
    assert_eq!(
        c.ctx.active_side(),
        1,
        "out-of-range active panel is clamped"
    );
}
