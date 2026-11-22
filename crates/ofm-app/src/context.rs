//! `AppContext`: the single shared state object of the UI (main thread only, always behind `Rc`).
//! Controllers, the command registry and the theme live here; widgets get an `Rc<AppContext>` (or
//! a `Weak`) from the install hooks. Parallel tasks add their own state through [`Extensions`]
//! instead of adding fields here.

use std::any::{Any, TypeId};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use ofm_config::{Config, ConfigPaths, Keymap};
use ofm_core::{CommandId, Location};
use ofm_vfs::VfsRegistry;
use ofm_vfs::pool::IoPool;

use crate::dispatch::{CommandDispatcher, ExecResult};
use crate::panel::archive::ArchiveService;
use crate::panel::{PanelController, PanelHost, Side};
use crate::signal::Signal;
use crate::theme::ThemeHost;
use crate::window::MainWindow;

/// Cheaply cloneable handles to the back-end services (all `Send + Sync` inside).
#[derive(Clone)]
pub struct Services {
    /// Provider lookup (local now; archive/list providers are registered by later milestones).
    pub vfs: VfsRegistry,
    /// Shared I/O pool (4 threads) for listing/stat/preview work.
    pub pool: IoPool,
    /// Archive provider, passwords and temp directories (F26); its provider is registered in
    /// `vfs` for `Location::Archive`.
    pub archives: ArchiveService,
}

impl Services {
    /// Production services: local provider + 4 I/O threads.
    pub fn new() -> Self {
        let archives = ArchiveService::new();
        let mut vfs = VfsRegistry::with_local();
        archives.register(&mut vfs);
        Services {
            vfs,
            pool: IoPool::new(4),
            archives,
        }
    }
}

impl Default for Services {
    fn default() -> Self {
        Self::new()
    }
}

/// Type-keyed bag for state owned by a task's module (e.g. `navigation` stores its hotlist).
/// Insert in the module's `install_state` hook, read with `ctx.ext.get::<T>()`.
#[derive(Default)]
pub struct Extensions {
    map: RefCell<HashMap<TypeId, Rc<dyn Any>>>,
}

impl Extensions {
    /// Store `value`, replacing a previous value of the same type.
    pub fn insert<T: 'static>(&self, value: Rc<T>) {
        self.map.borrow_mut().insert(TypeId::of::<T>(), value);
    }

    /// Fetch the value of type `T`, if any.
    pub fn get<T: 'static>(&self) -> Option<Rc<T>> {
        let any = self.map.borrow().get(&TypeId::of::<T>()).cloned()?;
        any.downcast::<T>().ok()
    }
}

/// Everything needed to build an [`AppContext`]; tests construct this directly.
pub struct AppInit {
    /// Where config/state files live.
    pub paths: ConfigPaths,
    /// Initial location of the left panel.
    pub left: Location,
    /// Initial location of the right panel.
    pub right: Location,
}

/// Shared UI state. Create with [`crate::app::build_context`].
pub struct AppContext {
    /// Back-end services.
    pub services: Services,
    /// XDG paths in use.
    pub paths: ConfigPaths,
    /// Current configuration (reloaded by `app.reload_config`).
    pub config: RefCell<Config>,
    /// Current keymap (swapped as a whole on reload).
    pub keymap: RefCell<Rc<Keymap>>,
    /// Theme + render palette + generated CSS.
    pub theme: Rc<ThemeHost>,
    /// Left and right panel sides.
    pub panels: [Rc<PanelHost>; 2],
    /// Emitted with the new side when the active panel changes.
    pub active_changed: Signal<Side>,
    /// Module-owned state of parallel tasks.
    pub ext: Extensions,
    active: Cell<Side>,
    dispatcher: OnceCell<Rc<CommandDispatcher>>,
    window: OnceCell<Rc<MainWindow>>,
    modal_depth: Cell<u32>,
    cmdline_has_text: Cell<bool>,
    terminal_focused: Cell<bool>,
}

impl AppContext {
    /// Assemble a context from already-loaded parts (see `app::build_context`).
    pub fn new(
        services: Services,
        paths: ConfigPaths,
        config: Config,
        keymap: Keymap,
        theme: Rc<ThemeHost>,
        init_left: Location,
        init_right: Location,
    ) -> Rc<Self> {
        let panels = [
            PanelHost::new(services.clone(), init_left),
            PanelHost::new(services.clone(), init_right),
        ];
        Rc::new(AppContext {
            services,
            paths,
            config: RefCell::new(config),
            keymap: RefCell::new(Rc::new(keymap)),
            theme,
            panels,
            active_changed: Signal::new(),
            ext: Extensions::default(),
            active: Cell::new(crate::panel::LEFT),
            dispatcher: OnceCell::new(),
            window: OnceCell::new(),
            modal_depth: Cell::new(0),
            cmdline_has_text: Cell::new(false),
            terminal_focused: Cell::new(false),
        })
    }

    // ---- panels -------------------------------------------------------------------------------
    /// Index of the active panel side.
    pub fn active_side(&self) -> Side {
        self.active.get()
    }

    /// Make `side` the active panel (clamped to 0/1) and notify observers.
    pub fn set_active_side(&self, side: Side) {
        let side = side.min(1);
        if self.active.replace(side) != side {
            self.active_changed.emit(&side);
        }
    }

    /// The other side.
    pub fn other_side(&self) -> Side {
        1 - self.active.get()
    }

    /// Host (tabs) of `side`.
    pub fn host(&self, side: Side) -> &Rc<PanelHost> {
        &self.panels[side.min(1)]
    }

    /// Active tab of the active panel: the target of most commands.
    pub fn active_controller(&self) -> Rc<PanelController> {
        self.host(self.active_side()).active()
    }

    /// Active tab of the inactive panel (copy/move target, swap, compare).
    pub fn other_controller(&self) -> Rc<PanelController> {
        self.host(self.other_side()).active()
    }

    /// Start listing both panels' initial locations (call after the window exists). A tab that
    /// is already loading (a restored session tab, F33) is left alone: reloading it would
    /// replace its wanted cursor name with the (not yet existing) cursor of the empty model.
    pub fn start_loading(&self) {
        for host in &self.panels {
            let tab = host.active();
            if tab.state() != crate::panel::LoadState::Loading {
                tab.reload();
            }
        }
    }

    // ---- dispatch -----------------------------------------------------------------------------
    /// Install the dispatcher (once, by `app::build_context`).
    pub fn set_dispatcher(&self, d: Rc<CommandDispatcher>) {
        if self.dispatcher.set(d).is_err() {
            tracing::error!("dispatcher installed twice");
        }
    }

    /// Run a command. This is what keys, menus, F-key bar clicks and tests all call.
    pub fn execute(self: &Rc<Self>, cmd: CommandId) -> ExecResult {
        match self.dispatcher.get() {
            Some(d) => d.execute(self, cmd),
            None => ExecResult::NotImplemented,
        }
    }

    // ---- window -------------------------------------------------------------------------------
    /// Register the main window (once, by `MainWindow::new`).
    pub fn set_window(&self, w: Rc<MainWindow>) {
        if self.window.set(w).is_err() {
            tracing::error!("main window registered twice");
        }
    }

    /// The main window, once built (`None` in headless tests).
    pub fn window(&self) -> Option<&Rc<MainWindow>> {
        self.window.get()
    }

    // ---- dispatcher state ---------------------------------------------------------------------
    /// A modal dialog is open: keys resolve in the `dialog` keymap section. Hold the guard while
    /// the dialog is shown.
    pub fn enter_modal(self: &Rc<Self>) -> ModalGuard {
        self.modal_depth.set(self.modal_depth.get() + 1);
        ModalGuard(Rc::clone(self))
    }

    /// True while any modal dialog is open.
    pub fn modal_open(&self) -> bool {
        self.modal_depth.get() > 0
    }

    /// M4 sets this when the command line gets/loses text (enables the `cmdline` keymap section).
    pub fn set_cmdline_has_text(&self, has_text: bool) {
        self.cmdline_has_text.set(has_text);
    }

    /// Whether the command line currently has text.
    pub fn cmdline_has_text(&self) -> bool {
        self.cmdline_has_text.get()
    }

    /// T4.1 sets this while the VTE terminal has the keyboard focus: key resolution then
    /// consults only the `global` keymap section (everything else belongs to the shell).
    pub fn set_terminal_focused(&self, focused: bool) {
        self.terminal_focused.set(focused);
    }

    /// Whether the VTE terminal currently has the keyboard focus.
    pub fn terminal_focused(&self) -> bool {
        self.terminal_focused.get()
    }
}

/// RAII marker returned by [`AppContext::enter_modal`].
pub struct ModalGuard(Rc<AppContext>);

impl Drop for ModalGuard {
    fn drop(&mut self) {
        let d = &self.0.modal_depth;
        d.set(d.get().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_roundtrip() {
        let e = Extensions::default();
        assert!(e.get::<u32>().is_none());
        e.insert(Rc::new(5u32));
        e.insert(Rc::new("x".to_owned()));
        assert_eq!(e.get::<u32>().as_deref(), Some(&5));
        assert_eq!(e.get::<String>().as_deref().map(String::as_str), Some("x"));
    }
}
