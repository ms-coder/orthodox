//! Session save/restore (F33): panels, tabs, paths, cursor names, sort, hidden flag, window size,
//! split ratio, one-panel mode. Storage is `ofm_config::Session` (`session.toml`).
//!
//! The file is read once at startup (the allowed startup exception) and written on a worker thread
//! when the window is closed (`app.quit` -> close request -> save -> destroy).

use std::cell::Cell;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use ofm_config::session::Session;
use ofm_config::session::WindowState;
use ofm_core::Location;

use super::layout::LayoutState;
use crate::context::AppContext;
use crate::navigation;
use crate::panel::{LoadOpts, LoadState, PanelController};
use crate::widgets::file_list::{self, ViewMode};
use crate::wiring;

/// Smallest window size a session may restore (a corrupt file must not give an unusable window).
const MIN_WINDOW: i32 = 320;
/// Largest window size a session may restore.
const MAX_WINDOW: i32 = 16_384;
/// How many parent directories a restored tab tries when its directory is gone.
const FALLBACK_LEVELS: u32 = 64;

/// Session bookkeeping in `ctx.ext`.
pub struct SessionState {
    /// What was loaded at startup (validated).
    pub loaded: Session,
    /// A close-time save is running (close requests are ignored meanwhile).
    pub closing: Cell<bool>,
}

/// Reads the view mode of a tab.
pub type ViewModeGetter = Box<dyn Fn(&PanelController) -> Option<String>>;
/// Applies a view mode to a tab.
pub type ViewModeSetter = Box<dyn Fn(&PanelController, &str)>;

/// Validate a freshly loaded session (deferred M1 finding): `active_panel` is clamped to 0/1, tabs
/// without an absolute path are dropped (a relative path would depend on the working directory), `active_tab` is clamped to the tab count, at most two panels are
/// kept and geometry values are made sane. Never fails: a damaged session degrades to defaults.
pub fn sanitize(s: &mut Session) {
    s.active_panel = s.active_panel.min(1);
    s.panels.truncate(2);
    if !(0.05..=0.95).contains(&s.split_ratio) {
        s.split_ratio = 0.5;
    }
    for p in &mut s.panels {
        // Keep `active_tab` pointing at the same tab after dropping the unusable ones.
        let active = p.active_tab;
        let mut kept = Vec::with_capacity(p.tabs.len());
        let mut new_active = None;
        for (i, tab) in std::mem::take(&mut p.tabs).into_iter().enumerate() {
            if !tab.path_buf().is_some_and(|p| p.is_absolute()) {
                continue;
            }
            if i >= active && new_active.is_none() {
                new_active = Some(kept.len());
            }
            kept.push(tab);
        }
        p.active_tab = new_active.unwrap_or(kept.len().saturating_sub(1));
        p.tabs = kept;
    }
    let w = &mut s.window;
    if !(MIN_WINDOW..=MAX_WINDOW).contains(&w.width)
        || !(MIN_WINDOW..=MAX_WINDOW).contains(&w.height)
    {
        w.width = 0;
        w.height = 0;
    }
}

/// True when the command line names directories explicitly (`ofm LEFT RIGHT`): then the stored
/// paths must not override them (window size and layout are still restored). Reading `argv` is no
/// file I/O.
fn cli_overrides_paths() -> bool {
    std::env::args_os().nth(1).is_some()
}

/// Startup: load and validate the session and remember it in `ctx.ext`. The panels are restored
/// later, once the window exists ([`restore_on_start`]).
pub fn install_state(ctx: &Rc<AppContext>) {
    let (mut session, warnings) = Session::load(&ctx.paths);
    for w in &warnings {
        tracing::warn!(file = ?w.file, "{}", w.message);
    }
    sanitize(&mut session);
    ctx.ext.insert(Rc::new(LayoutState::from_session(&session)));
    ctx.ext.insert(Rc::new(SessionState {
        loaded: session,
        closing: Cell::new(false),
    }));
}

/// Window exists: restore the panels of the loaded session (unless the command line named the
/// directories).
pub fn restore_on_start(ctx: &Rc<AppContext>) {
    let Some(state) = ctx.ext.get::<SessionState>() else {
        return;
    };
    if !cli_overrides_paths() {
        restore_panels(ctx, &state.loaded);
    }
}

/// Recreate the tabs of both panels from `session` (must be sanitized) with
/// `navigation::session::restore` (sort, hidden, lock, cursor), then the view modes, and make the
/// stored panel active. A restored directory that is gone falls back to its nearest ancestor.
pub fn restore_panels(ctx: &Rc<AppContext>, session: &Session) {
    for (side, panel) in session.panels.iter().enumerate() {
        if panel.tabs.is_empty() {
            continue;
        }
        let host = ctx.host(side);
        let base = host.active_index();
        navigation::session::restore(host, panel);
        for (ctrl, tab) in host.tabs().iter().skip(base).zip(&panel.tabs) {
            if let Some(mode) = ViewMode::parse(&tab.view) {
                file_list::set_view_mode(ctrl, mode);
            }
            guard_missing_dir(ctrl);
        }
    }
    ctx.set_active_side(usize::from(session.active_panel));
}

/// A restored directory may have disappeared: fall back to the nearest existing ancestor (the
/// listing error is the only signal, so no `stat` on the main thread).
fn guard_missing_dir(ctrl: &Rc<PanelController>) {
    let weak = Rc::downgrade(ctrl);
    let id: Rc<Cell<Option<crate::signal::SignalId>>> = Rc::new(Cell::new(None));
    let levels = Rc::new(Cell::new(0u32));
    let id2 = Rc::clone(&id);
    let sid = ctrl.changed.connect(move |_| {
        let Some(ctrl) = weak.upgrade() else { return };
        match ctrl.state() {
            LoadState::Loading => {}
            LoadState::Idle => {
                if let Some(i) = id2.take() {
                    ctrl.changed.disconnect(i);
                }
            }
            LoadState::Failed(_) => {
                let parent = ctrl.location().local_path().and_then(|p| p.parent()).map(PathBuf::from);
                match parent {
                    Some(p) if levels.get() < FALLBACK_LEVELS => {
                        levels.set(levels.get() + 1);
                        tracing::info!(to = %p.display(), "restored directory is gone; using parent");
                        ctrl.load(
                            Location::Local(p),
                            LoadOpts {
                                skip_history: true,
                                ..LoadOpts::default()
                            },
                        );
                    }
                    _ => {
                        if let Some(i) = id2.take() {
                            ctrl.changed.disconnect(i);
                        }
                    }
                }
            }
        }
    });
    id.set(Some(sid));
}

/// Capture the current state (main thread, memory only).
pub fn snapshot(ctx: &AppContext) -> Session {
    let previous = ctx
        .ext
        .get::<SessionState>()
        .map(|s| s.loaded.clone())
        .unwrap_or_default();
    let mut panels = Vec::new();
    for side in 0..2 {
        let host = ctx.host(side);
        let mut ps = navigation::session::capture(host);
        // `capture` skips panelized lists; the view modes (owned by the list widget) follow the
        // same order.
        let local = host
            .tabs()
            .into_iter()
            .filter(|c| !matches!(c.location(), Location::List { .. }));
        for (tab, ctrl) in ps.tabs.iter_mut().zip(local) {
            tab.view = file_list::view_mode(&ctrl).as_str().to_owned();
        }
        panels.push(ps);
    }
    let (split_ratio, one_panel) = ctx
        .ext
        .get::<LayoutState>()
        .map_or((previous.split_ratio, previous.one_panel), |l| {
            (l.ratio(), l.one_panel())
        });
    let mut window = previous.window.clone();
    if let Some(w) = ctx.window() {
        let win = &w.window;
        window.maximized = win.is_maximized();
        // The size of a maximized/fullscreen window is not the window's own size: keep the old one.
        if !win.is_maximized() && !win.is_fullscreen() {
            // `default-width/height` follow user resizes and exclude the client-side shadow, so
            // it round-trips through `set_default_size` (the widget size would shrink the window
            // by the shadow on every restart).
            let (dw, dh) = win.default_size();
            let (width, height) = if dw >= MIN_WINDOW && dh >= MIN_WINDOW {
                (dw, dh)
            } else {
                (win.width(), win.height())
            };
            if width >= MIN_WINDOW && height >= MIN_WINDOW {
                window = WindowState {
                    width,
                    height,
                    maximized: false,
                };
            }
        }
    }
    Session {
        window,
        split_ratio,
        one_panel,
        active_panel: u8::try_from(ctx.active_side().min(1)).unwrap_or(0),
        panels,
        ..previous
    }
}

/// Save the session on the I/O pool, then call `done` on the main thread (also when saving
/// failed: quitting must never hang on a read-only state directory).
pub fn save_async(ctx: &Rc<AppContext>, done: impl FnOnce() + 'static) {
    let session = snapshot(ctx);
    let paths = ctx.paths.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(session.save(&paths));
    });
    let done = Rc::new(Cell::new(Some(done)));
    let done_closed = Rc::clone(&done);
    wiring::attach(
        rx,
        move |res| {
            if let Err(e) = res {
                tracing::warn!(error = %e, "cannot save session");
            }
            ControlFlow::Break(())
        },
        move || {
            if let Some(f) = done_closed.take() {
                f();
            }
        },
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_config::session::{PanelSession, TabSession};

    use super::*;

    fn tab(path: &str) -> TabSession {
        let mut t = TabSession::default();
        t.set_path(std::path::Path::new(path));
        t
    }

    #[test]
    fn clamps_active_panel_and_tab() {
        let mut s = Session {
            active_panel: 7,
            panels: vec![
                PanelSession {
                    active_tab: 9,
                    tabs: vec![tab("/a"), tab("/b")],
                },
                PanelSession {
                    active_tab: 3,
                    tabs: vec![],
                },
                PanelSession::default(),
            ],
            ..Session::default()
        };
        sanitize(&mut s);
        assert_eq!(s.active_panel, 1);
        assert_eq!(s.panels.len(), 2);
        assert_eq!(s.panels[0].active_tab, 1);
        assert_eq!(s.panels[1].active_tab, 0);
    }

    #[test]
    fn dropping_pathless_tabs_keeps_active_tab_on_the_same_tab() {
        let mut s = Session {
            panels: vec![PanelSession {
                active_tab: 2,
                tabs: vec![TabSession::default(), tab("/a"), tab("/b"), tab("/c")],
            }],
            ..Session::default()
        };
        sanitize(&mut s);
        let p = &s.panels[0];
        assert_eq!(p.tabs.len(), 3);
        assert_eq!(
            p.tabs[p.active_tab].path_buf().unwrap(),
            PathBuf::from("/b")
        );

        // The active tab itself had no path: the next usable one is used.
        let mut s = Session {
            panels: vec![PanelSession {
                active_tab: 0,
                tabs: vec![TabSession::default(), tab("/a")],
            }],
            ..Session::default()
        };
        sanitize(&mut s);
        assert_eq!(s.panels[0].active_tab, 0);
        assert_eq!(s.panels[0].tabs.len(), 1);
    }

    #[test]
    fn relative_paths_are_dropped() {
        let mut s = Session {
            panels: vec![PanelSession {
                active_tab: 1,
                tabs: vec![tab("rel/dir"), tab("/abs"), tab("")],
            }],
            ..Session::default()
        };
        sanitize(&mut s);
        let p = &s.panels[0];
        assert_eq!(p.tabs.len(), 1);
        assert_eq!(p.tabs[0].path_buf().unwrap(), PathBuf::from("/abs"));
        assert_eq!(p.active_tab, 0);
    }

    #[test]
    fn sanitizes_geometry() {
        let mut s = Session {
            split_ratio: f32::NAN,
            window: WindowState {
                width: -5,
                height: 100_000,
                maximized: true,
            },
            ..Session::default()
        };
        sanitize(&mut s);
        assert_eq!(s.split_ratio, 0.5);
        assert_eq!((s.window.width, s.window.height), (0, 0));
        assert!(s.window.maximized);
        let mut ok = Session {
            window: WindowState {
                width: 1000,
                height: 700,
                maximized: false,
            },
            ..Session::default()
        };
        sanitize(&mut ok);
        assert_eq!((ok.window.width, ok.window.height), (1000, 700));
    }
}
