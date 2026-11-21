//! The list of known themes (`Theme::discover`), switching by id, system-colour tracking and live
//! reload of theme files (F30). State lives in `ctx.ext` as [`ThemeState`].

use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;
use std::rc::Rc;

use gtk::gdk;
use ofm_config::Theme;

use super::SystemColors;
use crate::context::AppContext;
use crate::wiring;

/// Themes available to `app.theme_next`; filled at startup, refreshed on reload.
#[derive(Default)]
pub struct ThemeState {
    themes: RefCell<Vec<Theme>>,
    /// A config save is running on the pool.
    saving: Cell<bool>,
    /// The config changed again while a save was running.
    save_again: Cell<bool>,
}

impl ThemeState {
    /// State with the given theme list.
    pub fn new(themes: Vec<Theme>) -> Self {
        ThemeState {
            themes: RefCell::new(themes),
            ..ThemeState::default()
        }
    }

    /// Replace the list (config reload, theme file changed).
    pub fn set_themes(&self, themes: Vec<Theme>) {
        *self.themes.borrow_mut() = themes;
    }

    /// Snapshot of the known themes in menu order.
    pub fn themes(&self) -> Vec<Theme> {
        self.themes.borrow().clone()
    }

    /// Theme with `id`.
    pub fn find(&self, id: &str) -> Option<Theme> {
        self.themes.borrow().iter().find(|t| t.id == id).cloned()
    }

    /// The theme after `current_id` in list order, wrapping around; the first one when
    /// `current_id` is unknown.
    pub fn next_after(&self, current_id: &str) -> Option<Theme> {
        let themes = self.themes.borrow();
        let idx = themes.iter().position(|t| t.id == current_id);
        let next = idx.map_or(0, |i| (i + 1) % themes.len().max(1));
        themes.get(next).cloned()
    }
}

/// Startup: discover themes (startup exception: reads the themes dir) and follow the system colour
/// scheme. Needs no window.
pub fn install_state(ctx: &Rc<AppContext>) {
    let (themes, warnings) = Theme::discover(&ctx.paths);
    for w in &warnings {
        tracing::warn!(file = ?w.file, "{}", w.message);
    }
    ctx.ext.insert(Rc::new(ThemeState::new(themes)));
    if gdk::Display::default().is_some() {
        let sm = adw::StyleManager::default();
        ctx.theme
            .set_system_colors(SystemColors::from_style_manager(&sm));
    }
}

/// Window exists: read the live libadwaita colours, keep following light/dark changes and start the
/// theme-file watcher.
pub fn install_ui(ctx: &Rc<AppContext>, win: &gtk::Window) {
    let refresh = {
        let weak = Rc::downgrade(ctx);
        let win = win.clone();
        Rc::new(move || {
            if let Some(ctx) = weak.upgrade() {
                let sm = adw::StyleManager::default();
                let sys = SystemColors::from_style_manager(&sm).refined_from_widget(&win);
                if ctx.theme.palette().system != sys {
                    ctx.theme.set_system_colors(sys);
                }
            }
        })
    };
    refresh();
    let sm = adw::StyleManager::default();
    {
        let refresh = Rc::clone(&refresh);
        sm.connect_dark_notify(move |_| refresh());
    }
    {
        let refresh = Rc::clone(&refresh);
        sm.connect_high_contrast_notify(move |_| refresh());
    }
    watch_theme_files(ctx);
}

/// Switch to the theme `id` (instant) and remember it in the config file on a worker thread.
/// Returns false when no such theme exists.
pub fn switch_to(ctx: &Rc<AppContext>, id: &str) -> bool {
    let Some(state) = ctx.ext.get::<ThemeState>() else {
        return false;
    };
    let Some(theme) = state.find(id) else {
        return false;
    };
    tracing::info!(theme = %theme.id, "theme switch");
    ctx.theme.set_theme(theme);
    persist_theme_name(ctx, id);
    true
}

/// Store `theme.name` in `config.toml` (atomic write on the I/O pool; never blocks the UI).
/// Saves are serialised: while one runs, later changes only mark the config dirty, so the file
/// always ends up with the last choice.
fn persist_theme_name(ctx: &Rc<AppContext>, id: &str) {
    if ctx.config.borrow().theme.name == id {
        return;
    }
    ctx.config.borrow_mut().theme.name = id.to_owned();
    save_config(ctx);
}

fn save_config(ctx: &Rc<AppContext>) {
    let Some(state) = ctx.ext.get::<ThemeState>() else {
        return;
    };
    if state.saving.replace(true) {
        state.save_again.set(true);
        return;
    }
    // Only the theme name is persisted, edited in place so the user's comments and ordering in a
    // hand-edited config.toml survive (T8.8 `save_theme_choice`).
    let name = ctx.config.borrow().theme.name.clone();
    let paths = ctx.paths.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(ofm_config::Config::save_theme_choice(&paths, &name));
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        |res| {
            if let Err(e) = res {
                tracing::warn!(error = %e, "cannot save config");
            }
            ControlFlow::Break(())
        },
        move || {
            let Some(ctx) = weak.upgrade() else { return };
            let Some(state) = ctx.ext.get::<ThemeState>() else {
                return;
            };
            state.saving.set(false);
            if state.save_again.replace(false) {
                save_config(&ctx);
            }
        },
    );
}

/// Re-discover themes on a worker and apply them (if the current theme still exists, the new
/// version of it is shown: edit a theme file and watch it change).
pub fn rediscover(ctx: &Rc<AppContext>) {
    let paths = ctx.paths.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(Theme::discover(&paths));
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |(themes, warnings)| {
            if let Some(ctx) = weak.upgrade() {
                for w in &warnings {
                    tracing::warn!(file = ?w.file, "{}", w.message);
                }
                apply_discovered(&ctx, themes);
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

/// Install a freshly discovered list and refresh the shown theme when its definition changed.
pub fn apply_discovered(ctx: &Rc<AppContext>, themes: Vec<Theme>) {
    let Some(state) = ctx.ext.get::<ThemeState>() else {
        return;
    };
    let current = ctx.theme.theme();
    let updated = themes.iter().find(|t| t.id == current.id).cloned();
    state.set_themes(themes);
    if let Some(t) = updated
        && t != *current
    {
        ctx.theme.set_theme(t);
    }
}

fn watch_theme_files(ctx: &Rc<AppContext>) {
    if !ctx.config.borrow().theme.live_reload {
        return;
    }
    // `watch_themes` only checks that the directory exists (startup exception).
    let watcher = match ofm_config::reload::watch_themes(&ctx.paths.themes_dir()) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "theme live reload unavailable");
            return;
        }
    };
    let weak = Rc::downgrade(ctx);
    let events = watcher.events.clone();
    // The watcher must live as long as the loop; moving it into the future keeps it alive.
    gtk::glib::spawn_future_local(async move {
        let _watcher = watcher;
        while events.recv().await.is_ok() {
            match weak.upgrade() {
                Some(ctx) => rediscover(&ctx),
                None => break,
            }
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn state() -> ThemeState {
        ThemeState::new(
            ofm_config::theme::BUILTIN_THEMES
                .iter()
                .filter_map(|id| Theme::builtin(id))
                .collect(),
        )
    }

    #[test]
    fn next_after_cycles_and_handles_unknown() {
        let s = state();
        assert_eq!(s.next_after("far-blue").unwrap().id, "commander-light");
        assert_eq!(s.next_after("commander-light").unwrap().id, "adwaita");
        assert_eq!(s.next_after("adwaita").unwrap().id, "far-blue");
        assert_eq!(s.next_after("nope").unwrap().id, "far-blue");
        assert!(ThemeState::default().next_after("x").is_none());
        assert!(s.find("adwaita").is_some() && s.find("zzz").is_none());
    }
}
