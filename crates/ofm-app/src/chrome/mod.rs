//! Window chrome: F-key bar, F9 menu bar, status line texts, `app.*` commands. Owner: T2.2
//! (everything under `chrome/`).
//!
//! Widgets go into the slots of `MainWindow` (`menubar_slot`, `fkeybar_slot`, `cmdline_slot`; the
//! per-panel status line lives in `window/panel_view.rs` and formats through [`statusline`]);
//! they are built from the command registry (`ofm_core::commands::ALL`) and the keymap, and run
//! commands via `ctx.execute(cmd)` only.

pub mod fkeybar;
mod labels;
pub mod menubar;
pub mod menus;
pub mod statusline;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use ofm_config::{Config, Keymap, Theme};
use ofm_core::cmd;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::signal::Signal;
use crate::theme::{self, ThemeState};
use crate::window::MainWindow;
use crate::wiring;

pub use fkeybar::FKeyBar;
pub use menubar::MenuBar;

/// Chrome state in `ctx.ext`.
#[derive(Default)]
pub struct ChromeState {
    /// The F-key bar once the window is built.
    pub fkeybar: RefCell<Option<Rc<FKeyBar>>>,
    /// The menu bar once the window is built.
    pub menubar: RefCell<Option<Rc<MenuBar>>>,
    /// Emitted after the keymap was replaced (`app.reload_config`).
    pub keymap_changed: Signal<()>,
}

/// Register `app.menu`, `app.theme_next`, `app.reload_config`. (`app.quit` and the window
/// commands are in `window/commands.rs`; `app.preferences` stays unimplemented until M9.)
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::APP_MENU, |ctx| {
        let bar = ctx
            .ext
            .get::<ChromeState>()
            .and_then(|s| s.menubar.borrow().clone());
        if let Some(bar) = bar {
            bar.toggle();
        }
    });
    reg.register(cmd::APP_THEME_NEXT, |ctx| {
        let Some(state) = ctx.ext.get::<ThemeState>() else {
            return;
        };
        let current = ctx.theme.theme();
        if let Some(next) = state.next_after(&current.id) {
            theme::registry::switch_to(ctx, &next.id);
        }
    });
    reg.register(cmd::APP_RELOAD_CONFIG, reload_config);
}

/// Create chrome state in `ctx.ext`.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(ChromeState::default()));
}

/// Build F-key bar / menu bar into the window slots.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    let Some(state) = ctx.ext.get::<ChromeState>() else {
        return;
    };
    let menubar = MenuBar::new(ctx);
    win.menubar_slot.append(&menubar.root);
    let fkeys = FKeyBar::new(ctx);
    win.fkeybar_slot.append(&fkeys.root);
    fkeybar::follow_modifiers(&win.window, &fkeys);
    win.cmdline_slot.add_css_class("ofm-cmdline");
    {
        let fkeys = Rc::clone(&fkeys);
        state.keymap_changed.connect(move |()| fkeys.refresh());
    }
    if std::env::var_os("OFM_TMP_MENU").is_some() {
        let m = Rc::clone(&menubar);
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            m.open(2)
        });
    }
    *state.menubar.borrow_mut() = Some(menubar);
    *state.fkeybar.borrow_mut() = Some(fkeys);
}

/// Everything `app.reload_config` loads on the worker.
struct Reloaded {
    config: Config,
    keymap: Keymap,
    themes: Vec<Theme>,
    warnings: Vec<ofm_config::Warning>,
}

/// `app.reload_config`: re-read config, keymap and themes on a worker, then swap them in.
fn reload_config(ctx: &Rc<AppContext>) {
    let paths = ctx.paths.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let (config, mut warnings) = Config::load(&paths);
        let (keymap, w) = Keymap::load(&paths, config.preset(), config.typing());
        warnings.extend(w);
        let (themes, w) = Theme::discover(&paths);
        warnings.extend(w);
        let _ = tx.send_blocking(Reloaded {
            config,
            keymap,
            themes,
            warnings,
        });
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |r| {
            if let Some(ctx) = weak.upgrade() {
                apply_reloaded(&ctx, r);
            }
            std::ops::ControlFlow::Break(())
        },
        || {},
    );
}

fn apply_reloaded(ctx: &Rc<AppContext>, r: Reloaded) {
    for w in &r.warnings {
        tracing::warn!(file = ?w.file, "{}", w.message);
    }
    let theme_name = r.config.theme.name.clone();
    *ctx.config.borrow_mut() = r.config;
    ctx.keymap.replace(Rc::new(r.keymap));
    theme::registry::apply_discovered(ctx, r.themes);
    if ctx.theme.theme().id != theme_name
        && let Some(t) = ctx
            .ext
            .get::<ThemeState>()
            .and_then(|s| s.find(&theme_name))
    {
        ctx.theme.set_theme(t);
    }
    if let Some(state) = ctx.ext.get::<ChromeState>() {
        state.keymap_changed.emit(&());
    }
    tracing::info!("configuration reloaded");
}
