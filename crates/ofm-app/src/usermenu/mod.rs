//! The user menu (F28, T8.2b): `menu.user` (F2) lists the entries of the active folder's
//! `.ofm-menu.toml` (only when trusted) and of the global `menu.toml`; Enter or a hotkey runs
//! one; `menu.edit` / `menu.edit_local` open the menu files in the configured editor.
//!
//! Layout: `load.rs` (blocking disk reads, run on the I/O pool, plus the pure view builder),
//! `run.rs` (panel snapshot, expansion, prompts, confirmation), `exec.rs` (the actual process),
//! and the dialogs in `dialogs/usermenu_*.rs`.
//!
//! Flow of F2: pool job `load_blocking` -> (untrusted local menu: trust prompt; Trust is stored
//! by a pool job and the menu reloads) -> menu dialog -> `run::start`. Nothing here touches the
//! file system on the GTK thread.

pub mod exec;
pub mod load;
pub mod run;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::OsString;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use ofm_config::menu::{TrustStore, install_default_global};
use ofm_core::cmd;

use crate::context::AppContext;
use crate::dialogs::usermenu_menu::{self, MenuActions};
use crate::dialogs::usermenu_trust::{self, Decision, Review};
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::wiring;
use load::{Loaded, LocalSource};

/// Module state in `ctx.ext`.
#[derive(Default)]
pub struct UserMenuState {
    /// `(folder, sha256)` of untrusted local menus the user chose to ignore this session.
    ignored: RefCell<HashSet<(PathBuf, String)>>,
    /// The open menu dialog.
    open: RefCell<Option<adw::Dialog>>,
    /// A load is in flight (F2 pressed twice must not open two menus).
    loading: Cell<bool>,
}

impl UserMenuState {
    /// Whether the menu dialog is showing or being prepared.
    pub fn busy(&self) -> bool {
        self.loading.get() || self.open.borrow().is_some()
    }

    /// The menu dialog while it is showing (tests look into it).
    pub fn open_dialog(&self) -> Option<adw::Dialog> {
        self.open.borrow().clone()
    }

    fn is_ignored(&self, dir: &std::path::Path, hash: &str) -> bool {
        self.ignored
            .borrow()
            .contains(&(dir.to_path_buf(), hash.to_owned()))
    }

    fn ignore(&self, dir: &std::path::Path, hash: &str) {
        self.ignored
            .borrow_mut()
            .insert((dir.to_path_buf(), hash.to_owned()));
    }

    fn close_open(&self) {
        let open = self.open.borrow().clone();
        if let Some(d) = open {
            d.close();
        }
    }
}

/// Register `menu.user`, `menu.edit`, `menu.edit_local`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::MENU_USER, open_menu);
    reg.register(cmd::MENU_EDIT, edit_global);
    reg.register(cmd::MENU_EDIT_LOCAL, edit_local);
}

/// Create the module state.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(UserMenuState::default()));
}

/// The state (created on demand, so tests that skip `install_state` still work).
pub fn state(ctx: &AppContext) -> Rc<UserMenuState> {
    ctx.ext.get::<UserMenuState>().unwrap_or_else(|| {
        let s = Rc::new(UserMenuState::default());
        ctx.ext.insert(Rc::clone(&s));
        s
    })
}

/// The active panel's local folder; shows a message when it is not one.
fn active_dir(ctx: &Rc<AppContext>, title: &str) -> Option<PathBuf> {
    let dir = ctx
        .active_controller()
        .location()
        .local_path()
        .map(PathBuf::from);
    if dir.is_none() {
        ui::message(
            ctx,
            title,
            tr("The user menu works in local folders; this panel shows an archive or a file list."),
        );
    }
    dir
}

/// The file under the cursor of the active panel (`None` on `..` or in an empty folder).
fn cursor_name(ctx: &AppContext) -> Option<OsString> {
    ctx.active_controller()
        .model()
        .cursor_entry()
        .map(|e| e.name.clone())
        .filter(|n| n.as_encoded_bytes() != b"..")
}

/// `menu.user`: load off-thread, then show the trust prompt and/or the menu.
fn open_menu(ctx: &Rc<AppContext>) {
    let st = state(ctx);
    if st.busy() {
        return;
    }
    let Some(dir) = active_dir(ctx, tr("User menu")) else {
        return;
    };
    let cursor = cursor_name(ctx);
    st.loading.set(true);
    let paths = ctx.paths.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(load::load_blocking(&paths, &dir));
    });
    let (ctx2, st2) = (Rc::clone(ctx), Rc::clone(&st));
    wiring::attach(
        rx,
        move |loaded| {
            st2.loading.set(false);
            present_loaded(&ctx2, loaded, cursor.clone(), false);
            ControlFlow::Break(())
        },
        {
            let st = Rc::clone(&st);
            move || st.loading.set(false)
        },
    );
}

/// Trust prompt (when needed) and then the menu. `force_review`: the user asked to look at an
/// ignored local menu again.
fn present_loaded(
    ctx: &Rc<AppContext>,
    loaded: Loaded,
    cursor: Option<OsString>,
    force_review: bool,
) {
    let st = state(ctx);
    if let LocalSource::Untrusted {
        hash,
        text,
        truncated,
        warnings,
    } = &loaded.local
        && (force_review || !st.is_ignored(&loaded.dir, hash))
    {
        let review = Review {
            dir: loaded.dir.clone(),
            hash: hash.clone(),
            text: text.clone(),
            truncated: *truncated,
            problems: warnings.iter().map(load::describe_warning).collect(),
        };
        let (ctx2, st2) = (Rc::clone(ctx), Rc::clone(&st));
        let (dir, hash) = (loaded.dir.clone(), hash.clone());
        usermenu_trust::present(ctx, &review, false, move |decision| match decision {
            Decision::Trust => trust_then_reopen(&ctx2, dir, hash),
            Decision::Ignore => {
                st2.ignore(&dir, &hash);
                show_menu(&ctx2, loaded, cursor);
            }
        });
        return;
    }
    show_menu(ctx, loaded, cursor);
}

fn show_menu(ctx: &Rc<AppContext>, loaded: Loaded, cursor: Option<OsString>) {
    let st = state(ctx);
    let ignored = match &loaded.local {
        LocalSource::Untrusted { hash, .. } => st.is_ignored(&loaded.dir, hash),
        _ => false,
    };
    let view = load::build_view(&loaded, cursor.as_deref(), ignored);
    let (ctx_run, ctx_review) = (Rc::clone(ctx), Rc::clone(ctx));
    let actions = MenuActions {
        on_run: Rc::new(move |entry| run::start(&ctx_run, entry.item)),
        on_review: Rc::new({
            let (loaded, cursor) = (loaded.clone(), cursor.clone());
            move || present_loaded(&ctx_review, loaded.clone(), cursor.clone(), true)
        }),
    };
    if let Some(dialog) = usermenu_menu::present(ctx, &view, actions) {
        *st.open.borrow_mut() = Some(dialog.clone());
        let st2 = Rc::clone(&st);
        dialog.connect_closed(move |_| {
            st2.open.borrow_mut().take();
        });
    }
}

/// Record the trust decision on the pool, then show the (now trusted) menu.
fn trust_then_reopen(ctx: &Rc<AppContext>, dir: PathBuf, hash: String) {
    let paths = ctx.paths.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let (mut store, _warnings) = TrustStore::load(&paths);
        let _ = tx.send_blocking(store.trust(&dir, &hash).map_err(|e| e.to_string()));
    });
    let ctx2 = Rc::clone(ctx);
    wiring::attach(
        rx,
        move |result| {
            match result {
                Ok(()) => open_menu(&ctx2),
                Err(e) => ui::message(
                    &ctx2,
                    tr("Local menu file"),
                    &format!("{}: {e}", tr("Cannot remember the trust decision")),
                ),
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

/// `menu.edit`: make sure the global menu file exists (default content, never overwritten) and
/// open it in the configured editor.
fn edit_global(ctx: &Rc<AppContext>) {
    state(ctx).close_open();
    let paths = ctx.paths.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let r = install_default_global(&paths)
            .map(|_| paths.menu_file())
            .map_err(|e| e.to_string());
        let _ = tx.send_blocking(r);
    });
    let ctx2 = Rc::clone(ctx);
    wiring::attach(
        rx,
        move |result| {
            match result {
                Ok(file) => crate::desktop::editor::open_in_editor(&ctx2, &file),
                Err(e) => ui::message(&ctx2, tr("Edit menu"), &e),
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

/// `menu.edit_local`: create a template `.ofm-menu.toml` in the active folder unless one exists
/// (`create_new`: never truncated) and open it in the editor.
fn edit_local(ctx: &Rc<AppContext>) {
    state(ctx).close_open();
    let Some(dir) = active_dir(ctx, tr("Edit local menu")) else {
        return;
    };
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let r = load::create_local_template(&dir)
            .map(|(file, _created)| file)
            .map_err(|e| format!("{}: {e}", dir.display()));
        let _ = tx.send_blocking(r);
    });
    let ctx2 = Rc::clone(ctx);
    wiring::attach(
        rx,
        move |result| {
            match result {
                Ok(file) => crate::desktop::editor::open_in_editor(&ctx2, &file),
                Err(e) => ui::message(
                    &ctx2,
                    tr("Edit local menu"),
                    &format!("{}: {e}", tr("Cannot create the menu file")),
                ),
            }
            ControlFlow::Break(())
        },
        || {},
    );
}
