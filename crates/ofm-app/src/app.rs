//! Application bootstrap: `adw::Application`, config/keymap/theme loading, context assembly.
//! Frozen core (T2.0); T2.2 adds session save/restore through the hooks, not here.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use adw::prelude::*;
use ofm_config::{Config, ConfigPaths, Keymap, Theme};
use ofm_core::Location;

use crate::context::{AppContext, AppInit, Services};
use crate::dispatch::{CommandDispatcher, CommandRegistry};
use crate::theme::ThemeHost;
use crate::window::MainWindow;

/// Context and main window, kept alive together for the lifetime of the application.
type LiveApp = (Rc<AppContext>, Rc<MainWindow>);

/// Reverse-DNS application id (matches the desktop file in M10).
pub const APP_ID: &str = "io.github.mscoder.Ofm";

/// Parse `ofm [LEFT_DIR [RIGHT_DIR]]`; relative paths are made absolute against `cwd`.
/// Missing arguments default to `cwd` (left) and `$HOME` (right), falling back to `/`.
pub fn initial_locations(
    args: &[OsString],
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
) -> (Location, Location) {
    let root = PathBuf::from("/");
    let base = cwd.unwrap_or_else(|| root.clone());
    let abs = |p: &OsString| -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_owned()
        } else {
            base.join(p)
        }
    };
    let left = args.first().map(abs).unwrap_or_else(|| base.clone());
    let right = args
        .get(1)
        .map(abs)
        .or(home)
        .unwrap_or_else(|| root.clone());
    (Location::Local(left), Location::Local(right))
}

/// Load config, keymap and theme from `init.paths` (startup exception to "no I/O on the main
/// thread": CLAUDE.md invariant 2), install all command handlers and module state. Needs
/// `gtk::init()` (the theme host creates a `CssProvider`) but no window.
pub fn build_context(init: AppInit) -> Rc<AppContext> {
    let (config, mut warnings) = Config::load(&init.paths);
    let (keymap, w) = Keymap::load(&init.paths, config.preset(), config.typing());
    warnings.extend(w);
    let (themes, w) = Theme::discover(&init.paths);
    warnings.extend(w);
    for w in &warnings {
        tracing::warn!(file = ?w.file, "{}", w.message);
    }
    let theme = themes
        .iter()
        .find(|t| t.id == config.theme.name)
        .cloned()
        .or_else(|| Theme::builtin("far-blue"))
        .unwrap_or_default();
    let theme_host = ThemeHost::new(theme);

    let ctx = AppContext::new(
        Services::new(),
        init.paths,
        config,
        keymap,
        theme_host,
        init.left,
        init.right,
    );
    let mut registry = CommandRegistry::new();
    crate::hooks::install_commands(&mut registry);
    if !registry.duplicates().is_empty() {
        tracing::error!(commands = ?registry.duplicates(), "commands registered by more than one module");
    }
    ctx.set_dispatcher(CommandDispatcher::new(registry));
    crate::hooks::install_state(&ctx);
    // Config problems must reach the user, not only the log: they are listed in the Preferences
    // and Help dialogs, and the status line shows a hint once the window exists
    // (`notice::show_pending` in `chrome::install_ui`).
    let n = warnings.len();
    crate::dialogs::preferences::record(&ctx, warnings);
    if n > 0 {
        crate::chrome::notice::show(&ctx, &crate::dialogs::preferences::problems_hint(n));
    }
    ctx
}

/// Entry point of the binary: parse arguments, run the GTK application.
pub fn run() -> anyhow::Result<ExitCode> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Startup only: getcwd/env lookups, not file I/O.
    let (left, right) = initial_locations(
        &args,
        std::env::current_dir().ok(),
        std::env::var_os("HOME").map(PathBuf::from),
    );

    let app = adw::Application::builder().application_id(APP_ID).build();
    // The UI closures hold only `Weak<AppContext>`, so this state is the context's owner: it must
    // live as long as the application (dropping it would kill every command and key binding).
    let state: Rc<RefCell<Option<LiveApp>>> = Rc::default();
    // Private temp directories of F3 on archive entries do not outlive the application.
    let exit_state = Rc::clone(&state);
    app.connect_shutdown(move |_| {
        if let Some((ctx, _)) = exit_state.borrow().as_ref() {
            ctx.services.archives.cleanup_all();
        }
    });
    app.connect_activate(move |app| {
        if let Some((_, win)) = state.borrow().as_ref() {
            win.present();
            return;
        }
        let ctx = build_context(AppInit {
            paths: ConfigPaths::from_env(),
            left: left.clone(),
            right: right.clone(),
        });
        ctx.theme.apply_css();
        let win = MainWindow::new(app, &ctx);
        ctx.start_loading();
        win.present();
        *state.borrow_mut() = Some((ctx, win));
    });
    // Our own positional arguments must not reach GApplication.
    let argv0 = std::env::args_os()
        .next()
        .unwrap_or_else(|| OsString::from("ofm"));
    let code = app.run_with_args(&[argv0.to_string_lossy().into_owned()]);
    Ok(code.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn initial_locations_rules() {
        let cwd = Some(PathBuf::from("/work"));
        let home = Some(PathBuf::from("/home/u"));
        let (l, r) = initial_locations(&[], cwd.clone(), home.clone());
        assert_eq!(l, Location::Local("/work".into()));
        assert_eq!(r, Location::Local("/home/u".into()));
        let (l, r) = initial_locations(&[os("a"), os("/b")], cwd.clone(), home);
        assert_eq!(l, Location::Local("/work/a".into()));
        assert_eq!(r, Location::Local("/b".into()));
        let (l, r) = initial_locations(&[], None, None);
        assert_eq!(l, Location::Local("/".into()));
        assert_eq!(r, Location::Local("/".into()));
    }
}
