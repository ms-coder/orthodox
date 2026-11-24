//! The embedded terminal (F12, ADR-0003). Owner: **T4.1** (`terminal/**`, plus
//! `crates/ofm-app/resources/shell/**`); T4.0 froze the public API and hook bodies below —
//! T4.1 implements them without changing signatures (report to the lead if one is wrong).
//!
//! Design summary (full rules and rationale in ADR-0003):
//! - One persistent shell per window (`[terminal] shell`, empty = `$SHELL`) in a VTE terminal,
//!   spawned in the active panel's directory. bash/zsh/fish are spawned through an injected
//!   rc wrapper that emits OSC 7 (`file://host/percent-encoded/path`) on every prompt; other
//!   shells spawn plainly with **no** sync in either direction (still parsed: OSC 7 arriving
//!   from the user's own rc moves the panel, but OFM never injects `cd` without idle detection).
//! - Shell phase ([`ShellState`]): `Busy` from spawn and from every fed input line until the
//!   next OSC 7 (or the child exiting → `Exited` → respawn, with a message in the scrollback).
//! - Panel → shell: on a location change of the active panel, if the shell is `Idle`, the
//!   command line is empty and the panel directory is local, feed
//!   [`ofm_core::cmdline::cd_command`] bytes (` cd -- '…'`, leading space = no history entry).
//! - Shell → panel: an accepted OSC 7 (host empty/localhost/own hostname) navigates the active
//!   panel via [`crate::navigation::goto::navigate`], but only when the directory differs
//!   (that is the fixed point that stops the sync loop).
//! - `app.terminal_toggle` (Ctrl+O) hides/shows `win.panels`; the command line row, menu bar
//!   and F-key bar stay visible; the terminal area expands into the panels' space.
//! - Terminal editors from F4 (M5, `editor.terminal_editors`) run through
//!   [`TerminalHandle::run_program`]: their own VTE widget and child process with the exact
//!   argv (no line typed into the shell, so it works for every shell, quoting-free).
//! - Keys: while the VTE widget has focus only the `global` keymap section is consulted
//!   (see OWNERSHIP.md "M4 lead follow-ups"); everything else goes to the shell.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ofm_core::cmd;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::signal::Signal;
use crate::window::{MainWindow, PanelView};

mod program;
mod shell;
mod ui;

/// Phase of the window's shell (ADR-0003; published as [`ShellHost`] in `ctx.ext`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellState {
    /// At a prompt: an OSC 7 was seen after the last fed line.
    #[default]
    Idle,
    /// A command is running: set when a line is fed (Enter in the command line, a cd sync, an
    /// F4 editor, or the user pressing Enter inside the terminal) and cleared by the next OSC 7.
    Busy,
    /// The shell process exited; the terminal respawns it.
    Exited,
}

/// The shell phase of the window, stored in `ctx.ext` by [`install_state`]; `changed` fires
/// whenever the phase actually changes (observers: command line, status line, F-key bar).
#[derive(Default)]
pub struct ShellHost {
    state: Cell<ShellState>,
    /// Emitted with the new state on every transition (never for a no-op `set`).
    pub changed: Signal<ShellState>,
}

impl ShellHost {
    /// Host starting in [`ShellState::Idle`] (the terminal flips to `Busy` while spawning).
    pub fn new() -> Self {
        Self::default()
    }

    /// Current phase.
    pub fn get(&self) -> ShellState {
        self.state.get()
    }

    /// Set the phase and emit [`ShellHost::changed`] when it differs from the current one.
    pub fn set(&self, state: ShellState) {
        if self.state.replace(state) != state {
            self.changed.emit(&state);
        }
    }
}

/// How other modules drive the embedded shell: the command line (Enter, insert-name), cd sync
/// and F4 terminal editors. Stored in `ctx.ext` by [`install_state`]; fetch with
/// [`handle`] — lazily, not during `install_state` (no startup ordering constraints).
/// The bodies delegate to the terminal UI once [`install_ui`] has built it (before that,
/// calls are logged and dropped).
pub struct TerminalHandle {
    host: Rc<ShellHost>,
    /// Set by [`install_ui`] (the VTE widget needs the window); `None` in headless tests.
    ui: RefCell<Option<Rc<ui::TerminalUi>>>,
}

impl TerminalHandle {
    /// Wrap a [`ShellHost`] (the terminal keeps both in `ctx.ext`).
    pub fn new(host: Rc<ShellHost>) -> Self {
        TerminalHandle {
            host,
            ui: RefCell::new(None),
        }
    }

    /// Feed exactly one input line to the shell: appends `b"\n"` unless the bytes already end
    /// with one, then writes them to the terminal child (VTE `feed_child`). Flips the shell to
    /// [`ShellState::Busy`] until its next OSC 7. No shell/no sync support: logged, dropped.
    pub fn send_line(&self, bytes: &[u8]) {
        match self.ui.borrow().as_ref() {
            Some(ui) => ui.send_line(bytes),
            None => tracing::debug!("terminal: send_line dropped (no terminal installed)"),
        }
    }

    /// Run `argv` as its own program (exact words, never typed into the shell, so no quoting
    /// and no shell-idle requirement) in a fresh terminal view that replaces the shell view
    /// and the panels until the program exits; then the panels come back. This is how F4 runs
    /// terminal editors. `cwd` is the working directory (non-UTF-8: inherited). `on_error`
    /// receives the message if the program cannot be started (after this call returned
    /// `Ok`). Errors: no terminal, or another program already running.
    pub fn run_program(
        &self,
        argv: &[std::ffi::OsString],
        cwd: Option<&std::path::Path>,
        on_error: impl Fn(String) + 'static,
    ) -> Result<(), String> {
        match self.ui.borrow().as_ref() {
            Some(ui) => ui.run_program(argv, cwd, Box::new(on_error)),
            None => Err(crate::i18n::tr("The terminal is not available.").to_owned()),
        }
    }

    /// [`run_program`](Self::run_program) that also calls `on_exit` with the raw wait status
    /// (0 = exited successfully) after the program exited and the panels are back. Not called
    /// when the program cannot be started (then `on_error` is).
    pub fn run_program_notify(
        &self,
        argv: &[std::ffi::OsString],
        cwd: Option<&std::path::Path>,
        on_error: impl Fn(String) + 'static,
        on_exit: impl FnOnce(i32) + 'static,
    ) -> Result<(), String> {
        match self.ui.borrow().as_ref() {
            Some(ui) => {
                ui.run_program_notify(argv, cwd, Box::new(on_error), Some(Box::new(on_exit)))
            }
            None => Err(crate::i18n::tr("The terminal is not available.").to_owned()),
        }
    }

    /// Show the terminal full size (hide the panels — what Ctrl+O or running a command does).
    pub fn show(&self) {
        match self.ui.borrow().as_ref() {
            Some(ui) => ui.show(),
            None => tracing::debug!("terminal: show dropped (no terminal installed)"),
        }
    }

    /// Show the panels again over the terminal.
    pub fn hide(&self) {
        match self.ui.borrow().as_ref() {
            Some(ui) => ui.hide(),
            None => tracing::debug!("terminal: hide dropped (no terminal installed)"),
        }
    }

    /// Whether the terminal (not the panels) is currently shown full size.
    pub fn is_visible(&self) -> bool {
        self.ui.borrow().as_ref().is_some_and(|ui| ui.is_visible())
    }

    /// How names inserted into the command line must be quoted for the running shell (fish
    /// differs from POSIX); POSIX before the terminal exists.
    pub fn quote_style(&self) -> ofm_core::cmdline::QuoteStyle {
        self.ui
            .borrow()
            .as_ref()
            .map(|ui| ui.quote_style())
            .unwrap_or_default()
    }

    /// Current shell phase (same value as [`shell_host`]'s `get`).
    pub fn shell_state(&self) -> ShellState {
        self.host.get()
    }
}

/// The window's [`TerminalHandle`] (`None` only before [`install_state`] ran).
pub fn handle(ctx: &AppContext) -> Option<Rc<TerminalHandle>> {
    ctx.ext.get::<TerminalHandle>()
}

/// The window's [`ShellHost`] (`None` only before [`install_state`] ran).
pub fn shell_host(ctx: &AppContext) -> Option<Rc<ShellHost>> {
    ctx.ext.get::<ShellHost>()
}

/// Register `app.terminal_toggle` (Ctrl+O): toggle the panels area, keep the command line
/// row, menu bar and F-key bar (ADR-0003 semantics live in `ui::TerminalUi::{show,hide}`).
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::APP_TERMINAL_TOGGLE, |ctx| {
        let Some(h) = handle(ctx) else {
            tracing::debug!("app.terminal_toggle: no terminal installed");
            return;
        };
        if h.is_visible() {
            h.hide();
        } else {
            h.show();
        }
    });
}

/// Create the [`ShellHost`] and [`TerminalHandle`] in `ctx.ext` and spawn the shell in the
/// active panel's directory (T4.1; the spawn itself happens in `install_ui`, once the
/// `terminal_slot` exists).
pub fn install_state(ctx: &Rc<AppContext>) {
    let host = Rc::new(ShellHost::new());
    ctx.ext
        .insert(Rc::new(TerminalHandle::new(Rc::clone(&host))));
    ctx.ext.insert(host);
}

/// Per-tab wiring for shell-follows-panel: subscribe to `PanelEvent::Location` of every tab and
/// to panel/tab switches (`PanelHost::active_changed`, `AppContext::active_changed`), then feed
/// [`ofm_core::cmdline::cd_command`] while the shell is idle and the command line is empty
/// (`AppContext::cmdline_has_text`).
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    ui::install_panel_view(ctx, view.side);
}

/// Build the VTE widget into `win.terminal_slot`, spawn the shell with the injected rc
/// wrapper, wire OSC 7 (`current-directory-uri-changed`) → [`ShellHost`] + panel navigation,
/// `child-exited` → respawn, and implement `app.terminal_toggle`.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    let Some(host) = shell_host(ctx) else {
        tracing::error!("terminal: install_state did not run before install_ui");
        return;
    };
    let term = ui::TerminalUi::new(ctx, win, host);
    if let Some(h) = ctx.ext.get::<TerminalHandle>() {
        *h.ui.borrow_mut() = Some(Rc::clone(&term));
    }
    ctx.ext.insert(term);
}
