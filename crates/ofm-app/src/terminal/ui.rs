//! The VTE terminal widget and the shell runtime of one window (ADR-0003, T4.1):
//! widget construction and spawning, OSC 7 wiring (`ShellState` transitions, panel
//! navigation, panel→shell `cd` sync with stale coalescing), Ctrl+O view toggling, focus
//! tracking and the terminal-internal clipboard keys. Created by [`super::install_ui`];
//! the frozen `mod.rs` API ([`super::TerminalHandle`]) delegates here.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use ofm_core::Location;
use ofm_core::cmdline::{QuoteStyle, cd_command_for, osc7_parse};
use vte4::prelude::{TerminalExt, TerminalExtManual};

use super::ShellState;
use super::program;
use super::shell::{self, RespawnGuard, ShellKind, SyncDecision};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::keys;
use crate::navigation::goto::navigate;
use crate::panel::{PanelController, PanelEvent, PanelHost, Side};
use crate::signal::SignalId;
use crate::widgets::file_list::FileList;
use crate::window::MainWindow;
use crate::wiring;

/// Everything the live terminal of a window needs. Stored in `ctx.ext` by
/// [`super::install_ui`] (after the window exists); holds only `Weak<AppContext>` so the
/// context owns the terminal, never the other way round.
pub(crate) struct TerminalUi {
    ctx: Weak<AppContext>,
    /// Own weak self, for callbacks on the widget's signals (set right after construction).
    self_weak: RefCell<Weak<TerminalUi>>,
    /// The VTE widget (always in `win.terminal_slot`, alive whether or not it is shown).
    term: vte4::Terminal,
    host: Rc<super::ShellHost>,
    window: adw::ApplicationWindow,
    panels: gtk::Paned,
    slot: gtk::Box,
    cmdline_slot: gtk::Box,
    lists: [FileList; 2],
    /// Resolved shell path (set when the spawn is prepared).
    shell: RefCell<PathBuf>,
    kind: Cell<ShellKind>,
    /// Per-session wrapper directory (`None` until written, or forever after a write error:
    /// the shell then runs plainly without sync).
    wrapper_dir: RefCell<Option<PathBuf>>,
    /// Machine hostname for OSC 7 acceptance, fetched once on the I/O pool.
    hostname: RefCell<Option<String>>,
    /// Last accepted OSC 7 directory (the shell's known cwd); `None` until the first OSC 7.
    cwd: RefCell<Option<PathBuf>>,
    /// A panel location change arrived while the shell was Busy: re-sync at the next OSC 7
    /// (ADR-0003 "rapid switches coalesce"); it also suppresses shell→panel navigation for
    /// that OSC 7 so a panel navigation is never undone.
    stale: Cell<bool>,
    /// The user typed something into the terminal that was not submitted yet (a half-typed
    /// line); a cd sync would be appended to it. Cleared by Enter, Ctrl-C and OSC 7.
    line_dirty: Cell<bool>,
    /// Detects a shell that exits again and again right after it was started.
    guard: RefCell<RespawnGuard>,
    /// Respawning was stopped (exit loop, or the spawn itself failed); the next terminal
    /// toggle or keypress in the terminal tries again.
    halted: Cell<bool>,
    /// The widget of a program (terminal editor) running instead of the shell view, if any.
    program: RefCell<Option<vte4::Terminal>>,
}

impl TerminalUi {
    /// Build the widget, wire every signal, prepare the session dir and spawn the shell.
    pub(crate) fn new(
        ctx: &Rc<AppContext>,
        win: &MainWindow,
        host: Rc<super::ShellHost>,
    ) -> Rc<Self> {
        let term = vte4::Terminal::new();
        style_terminal(ctx, &term);
        win.terminal_slot.append(&term);
        // Collapsed while the panels are up (Ctrl+O / `show()` reveals it); the widget and
        // the shell keep running hidden — showing is not re-spawning (ADR-0003).
        win.terminal_slot.set_visible(false);

        let ui = Rc::new(TerminalUi {
            ctx: Rc::downgrade(ctx),
            self_weak: RefCell::new(Weak::new()),
            term: term.clone(),
            host,
            window: win.window.clone(),
            panels: win.panels.clone(),
            slot: win.terminal_slot.clone(),
            cmdline_slot: win.cmdline_slot.clone(),
            lists: [win.left.list.clone(), win.right.list.clone()],
            shell: RefCell::new(PathBuf::new()),
            kind: Cell::new(ShellKind::Plain),
            wrapper_dir: RefCell::new(None),
            hostname: RefCell::new(None),
            cwd: RefCell::new(None),
            stale: Cell::new(false),
            line_dirty: Cell::new(false),
            guard: RefCell::new(RespawnGuard::default()),
            halted: Cell::new(false),
            program: RefCell::new(None),
        });
        *ui.self_weak.borrow_mut() = Rc::downgrade(&ui);

        ui.wire_signals();
        ui.wire_keys(ctx);
        ui.wire_panel_switches();
        ui.prepare_and_spawn();
        ui
    }

    fn weak(&self) -> Weak<TerminalUi> {
        self.self_weak.borrow().clone()
    }

    // ---- wiring ---------------------------------------------------------------------------

    fn wire_signals(&self) {
        // OSC 7: the shell reports its working directory.
        {
            let weak = self.weak();
            self.term
                .connect_current_directory_uri_changed(move |term| {
                    let Some(ui) = weak.upgrade() else { return };
                    if let Some(uri) = term.current_directory_uri() {
                        ui.on_osc7(uri.as_bytes());
                    }
                });
        }

        // The user typed into the terminal: Enter means a program may now be running, so no
        // cd sync until the next OSC 7 (ADR-0003); anything else leaves a half-typed line
        // that a fed `cd` would be appended to, until Enter / Ctrl-C / the next prompt.
        {
            let weak = self.weak();
            self.term.connect_commit(move |_, text, _| {
                let Some(ui) = weak.upgrade() else { return };
                ui.line_dirty
                    .set(shell::line_dirty_after(ui.line_dirty.get(), text));
                if text.contains('\r') && ui.host.get() != ShellState::Exited {
                    ui.host.set(ShellState::Busy);
                }
            });
        }

        // Child exit: notice into the scrollback, then respawn (same rules as the first
        // spawn). Every exit is logged. Three exits within about two seconds mean the shell
        // cannot start: stop, and restart only on the next toggle / keypress (ADR-0003).
        {
            let weak = self.weak();
            self.term.connect_child_exited(move |_, status| {
                let Some(ui) = weak.upgrade() else { return };
                ui.host.set(ShellState::Exited);
                *ui.cwd.borrow_mut() = None;
                ui.stale.set(false);
                ui.line_dirty.set(false);
                let looping = ui.guard.borrow_mut().record_exit(std::time::Instant::now());
                if looping {
                    tracing::warn!(status, "terminal: shell keeps exiting, respawn stopped");
                    ui.halted.set(true);
                    ui.feed_display(
                        format!(
                            "\r\n{}\r\n",
                            tr("[shell keeps exiting — press any key or Ctrl+O to try again]")
                        )
                        .as_bytes(),
                    );
                } else {
                    tracing::info!(status, "terminal: shell exited, respawning");
                    ui.feed_display(
                        format!("\r\n{}\r\n", tr("[process exited — restarting shell]")).as_bytes(),
                    );
                    ui.spawn_now();
                }
            });
        }

        // Pass the terminal's title through to the window (vim & co. set it).
        {
            let weak = self.weak();
            self.term.connect_window_title_changed(move |term| {
                let Some(ui) = weak.upgrade() else { return };
                let title = term.window_title().filter(|t| !t.is_empty());
                ui.window.set_title(title.as_deref());
            });
        }

        // Window gone: remove the per-session wrapper directory (off the main thread).
        {
            let weak = self.weak();
            self.window.connect_destroy(move |_| {
                if let Some(ui) = weak.upgrade()
                    && let Some(dir) = ui.wrapper_dir.borrow_mut().take()
                {
                    shell::remove_session_dir_async(dir);
                }
            });
        }
    }

    /// Focus tracking (drives key resolution: only `global` chords while the VTE has focus)
    /// and the terminal-internal clipboard keys — the one sanctioned key controller on a
    /// widget (text keys, not commands).
    fn wire_keys(&self, ctx: &Rc<AppContext>) {
        track_focus(ctx, &self.term);

        let weak = self.weak();
        let key_ctl = gtk::EventControllerKey::new();
        key_ctl.set_propagation_phase(gtk::PropagationPhase::Bubble);
        key_ctl.connect_key_pressed(move |_, keyval, keycode, state| {
            let Some(ui) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let Some(chord) = keys::chord_from_event(keyval, keycode, state) else {
                return glib::Propagation::Proceed;
            };
            if ui.halted.get() {
                // A real key (not a bare modifier) after the respawn loop was stopped.
                ui.restart_if_halted();
                return glib::Propagation::Stop;
            }
            use ofm_core::{Key, Mods};
            let Mods {
                ctrl: true,
                shift: true,
                alt: false,
                super_: false,
                ..
            } = chord.mods
            else {
                return glib::Propagation::Proceed;
            };
            let is = |k: char| matches!(chord.key, Key::Char(c) if c.eq_ignore_ascii_case(&k));
            if is('c') {
                ui.term.emit_copy_clipboard();
                glib::Propagation::Stop
            } else if is('v') {
                ui.term.paste_clipboard();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.term.add_controller(key_ctl);
    }

    /// Left/right panel switches change which directory is "the panel directory".
    fn wire_panel_switches(&self) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let weak_ctx = Rc::downgrade(&ctx);
        ctx.active_changed
            .connect(move |_| maybe_sync_from_ctx(&weak_ctx));
    }

    // ---- spawning -------------------------------------------------------------------------

    /// Resolve the shell, write the rc wrappers on the I/O pool (invariant 2), fetch the
    /// hostname on the way, then spawn. The shell is `Busy` from now on (ADR-0003), so a
    /// panel navigation during start-up is coalesced instead of injected into the rc phase.
    fn prepare_and_spawn(&self) {
        self.host.set(ShellState::Busy);
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let shell_path = shell::resolve_shell(
            &ctx.config.borrow().terminal.shell,
            std::env::var_os("SHELL")
                .map(|s| s.to_string_lossy().into_owned())
                .as_deref(),
        );
        // Known from the start so names typed before the spawn finished are quoted right.
        self.kind.set(ShellKind::of_path(&shell_path));
        *self.shell.borrow_mut() = shell_path;
        let base = shell::session_base();
        let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
            let prepared = shell::write_wrappers(&base);
            let host = shell::hostname();
            let _ = tx.send_blocking((prepared, host));
        });
        let weak = self.weak();
        wiring::attach(
            rx,
            move |(prepared, hostname)| {
                if let Some(ui) = weak.upgrade() {
                    *ui.wrapper_dir.borrow_mut() = prepared;
                    *ui.hostname.borrow_mut() = hostname;
                    ui.spawn_now();
                }
                std::ops::ControlFlow::Continue(())
            },
            || {},
        );
    }

    /// Spawn the shell in the active panel's directory (ADR-0003). Non-UTF-8 directories
    /// cannot be passed through `spawn_async`; the shell starts elsewhere and the first
    /// OSC 7 plus cd sync aligns it (for sync-capable shells).
    fn spawn_now(&self) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        self.host.set(ShellState::Busy);
        let kind = ShellKind::of_path(&self.shell.borrow());
        self.kind.set(kind);
        let wrapper_dir = self.wrapper_dir.borrow().clone();
        let argv = shell::build_argv(&self.shell.borrow(), kind, wrapper_dir.as_deref());
        let orig_zdotdir = std::env::var("ZDOTDIR").ok();
        let envv = shell::build_env(kind, wrapper_dir.as_deref(), orig_zdotdir.as_deref());
        let cwd = match ctx.active_controller().location() {
            Location::Local(p) => p,
            _ => PathBuf::from("/"),
        };
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let envv: Vec<&str> = envv.iter().map(String::as_str).collect();
        let weak = self.weak();
        tracing::debug!(shell = %self.shell.borrow().display(), ?kind, "terminal: spawning shell");
        self.term.spawn_async(
            vte4::PtyFlags::DEFAULT,
            cwd.to_str(),
            &argv,
            &envv,
            // `terminal.shell = "fish"` (no slash) must be found on $PATH.
            glib::SpawnFlags::SEARCH_PATH,
            || {},
            -1,
            None::<&gio::Cancellable>,
            move |res| match res {
                Ok(_) => tracing::debug!("terminal: shell started"),
                Err(e) => {
                    tracing::error!(error = %e, "terminal: could not start the shell");
                    if let Some(ui) = weak.upgrade() {
                        ui.host.set(ShellState::Exited);
                        // Retry on the next terminal toggle / keypress.
                        ui.halted.set(true);
                        ui.feed_display(
                            format!(
                                "\r\n{}\r\n",
                                tr(
                                    "[could not start the shell — check the terminal.shell setting; press any key or Ctrl+O to try again]"
                                )
                            )
                            .as_bytes(),
                        );
                    }
                }
            },
        );
    }

    // ---- OSC 7 and cd sync ----------------------------------------------------------------

    fn on_osc7(&self, uri: &[u8]) {
        if self.host.get() == ShellState::Exited {
            return; // respawn in flight
        }
        let Some((host, dir)) = osc7_parse(uri) else {
            tracing::debug!("terminal: ignoring a non-file OSC 7");
            return;
        };
        if !shell::accept_host(&host, self.hostname.borrow().as_deref()) {
            tracing::debug!(host = %host, "terminal: ignoring OSC 7 for another host");
            return;
        }
        *self.cwd.borrow_mut() = Some(dir.clone());
        self.line_dirty.set(false); // a fresh prompt: any half-typed line is gone
        self.host.set(ShellState::Idle);
        if self.stale.replace(false) {
            // The panel moved while the shell was busy: the panel wins (ADR-0003).
            self.maybe_sync();
        } else if let Some(ctx) = self.ctx.upgrade() {
            let ctrl = ctx.active_controller();
            if ctrl.location() != Location::Local(dir.clone()) {
                tracing::debug!(dir = %dir.display(), "terminal: shell moved, following with the panel");
                navigate(&ctx, &ctrl, Location::Local(dir), None);
            }
        }
    }

    /// Consider a panel→shell `cd` sync (called on panel location changes, panel/tab
    /// switches and when the terminal becomes visible; ADR-0003).
    pub(crate) fn maybe_sync(&self) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        if matches!(self.host.get(), ShellState::Busy | ShellState::Exited) {
            self.stale.set(true);
            return;
        }
        let dir = match ctx.active_controller().location() {
            Location::Local(p) => Some(p),
            _ => None,
        };
        match shell::should_sync(
            self.host.get(),
            self.kind.get().sync_capable(),
            ctx.cmdline_has_text(),
            self.line_dirty.get(),
            dir.as_deref(),
            self.cwd.borrow().as_deref(),
        ) {
            SyncDecision::FeedCd => {
                // `FeedCd` is only reachable with a local panel directory.
                let target = dir.unwrap_or_else(|| PathBuf::from("/"));
                self.stale.set(false);
                // `None`: the directory name holds control bytes the line editor would act
                // on (Ctrl-U, Ctrl-C, …) even inside quotes — never feed those.
                match cd_command_for(self.kind.get().quote_style(), &target) {
                    Some(line) => {
                        tracing::debug!(dir = %target.display(), "terminal: feeding a cd sync");
                        self.feed_child_bytes(&line);
                        self.host.set(ShellState::Busy);
                    }
                    None => tracing::debug!(
                        dir = %target.display(),
                        "terminal: cd sync skipped, the directory name contains control bytes"
                    ),
                }
            }
            SyncDecision::AlreadyThere => self.stale.set(false),
            SyncDecision::Blocked => {}
        }
    }

    // ---- feeding and view toggling --------------------------------------------------------

    /// [`super::TerminalHandle::send_line`]: exactly one input line, then Busy.
    pub(crate) fn send_line(&self, bytes: &[u8]) {
        if self.host.get() == ShellState::Exited {
            tracing::debug!("terminal: send_line dropped, the shell exited");
            return;
        }
        let mut line = bytes.to_vec();
        if !line.ends_with(b"\n") {
            line.push(b'\n');
        }
        self.feed_child_bytes(&line);
        self.host.set(ShellState::Busy);
    }

    fn feed_child_bytes(&self, bytes: &[u8]) {
        self.term.feed_child(bytes);
    }

    /// Display-only text (not sent to the child): the exit notice lands in the scrollback.
    fn feed_display(&self, bytes: &[u8]) {
        self.term.feed(bytes);
    }

    /// Show the terminal full size by hiding the panels (Ctrl+O / a command started from
    /// the command line). The command line row, menu bar and F-key bar stay (ADR-0003).
    pub(crate) fn show(&self) {
        self.restart_if_halted();
        self.panels.set_visible(false);
        self.slot.set_visible(true);
        // The terminal was hidden, so the focus is never inside it — it sits on a panel, a
        // chrome widget (menu/F-key buttons) or nowhere: Far keeps typing possible, so move
        // it to the command line entry (T4.2) if one exists, else the terminal.
        if let Some(entry) = self.cmdline_entry() {
            entry.grab_focus();
        } else {
            self.term.grab_focus();
        }
        self.maybe_sync();
    }

    /// [`super::TerminalHandle::run_program`]: run `argv` (exact words, no shell) in a fresh
    /// VTE widget that takes the shell view's place until the program exits; the panels come
    /// back then. `on_error` gets the message when the program cannot be started.
    pub(crate) fn run_program(
        &self,
        argv: &[OsString],
        cwd: Option<&Path>,
        on_error: Box<dyn Fn(String)>,
    ) -> Result<(), String> {
        self.run_program_notify(argv, cwd, on_error, None)
    }

    /// [`run_program`](Self::run_program) that also calls `on_exit` with the raw wait status
    /// once the program has exited and the panels are back (never when it could not be started:
    /// `on_error` is the only callback then). Used by callers that continue with the program's
    /// result (batch rename reads the edited file).
    pub(crate) fn run_program_notify(
        &self,
        argv: &[OsString],
        cwd: Option<&Path>,
        on_error: Box<dyn Fn(String)>,
        on_exit: Option<Box<dyn FnOnce(i32)>>,
    ) -> Result<(), String> {
        let Some(ctx) = self.ctx.upgrade() else {
            return Err(tr("The terminal is not available.").to_owned());
        };
        if argv.is_empty() {
            return Err(tr("Nothing to run.").to_owned());
        }
        if self.program.borrow().is_some() {
            return Err(tr("A program is already running in the terminal.").to_owned());
        }
        let plan = program::spawn_argv(argv);
        let plan: Vec<&str> = plan.iter().map(String::as_str).collect();
        let term = vte4::Terminal::new();
        style_terminal(&ctx, &term);
        track_focus(&ctx, &term);
        add_clipboard_keys(&term);
        *self.program.borrow_mut() = Some(term.clone());
        self.term.set_visible(false);
        self.slot.append(&term);
        self.panels.set_visible(false);
        self.slot.set_visible(true);
        term.grab_focus();

        let weak = self.weak();
        let done_term = term.clone();
        let exit_cb = Rc::new(RefCell::new(on_exit));
        let exit_cb_exited = Rc::clone(&exit_cb);
        term.connect_child_exited(move |_, status| {
            if let Some(ui) = weak.upgrade() {
                ui.program_finished(&done_term);
            }
            let cb = exit_cb_exited.borrow_mut().take();
            if let Some(cb) = cb {
                cb(status);
            }
        });
        let weak = self.weak();
        let fail_term = term.clone();
        let envv = ["TERM=xterm-256color"];
        term.spawn_async(
            vte4::PtyFlags::DEFAULT,
            cwd.and_then(Path::to_str), // a non-UTF-8 directory: inherit ours
            &plan,
            &envv,
            glib::SpawnFlags::SEARCH_PATH,
            || {},
            -1,
            None::<&gio::Cancellable>,
            move |res| {
                if let Err(e) = res {
                    tracing::error!(error = %e, "terminal: could not start the program");
                    // `on_error` reports it; `on_exit` never runs for a program that did not start.
                    drop(exit_cb.borrow_mut().take());
                    if let Some(ui) = weak.upgrade() {
                        ui.program_finished(&fail_term);
                    }
                    on_error(e.to_string());
                }
            },
        );
        Ok(())
    }

    /// The program's widget goes away; the shell view and the panels come back.
    fn program_finished(&self, term: &vte4::Terminal) {
        if self.program.borrow().as_ref() != Some(term) {
            return; // already cleaned up (a failed spawn can also report an exit)
        }
        *self.program.borrow_mut() = None;
        let had_focus = gtk::prelude::GtkWindowExt::focus(&self.window)
            .is_some_and(|f| f == *term.upcast_ref::<gtk::Widget>() || f.is_ancestor(term));
        self.slot.remove(term);
        self.term.set_visible(true);
        self.slot.set_visible(false);
        self.panels.set_visible(true);
        if had_focus && let Some(ctx) = self.ctx.upgrade() {
            ctx.set_terminal_focused(false);
            self.lists[ctx.active_side()].grab_focus();
        }
    }

    /// Start the shell again after the respawn loop was stopped or a spawn failed.
    fn restart_if_halted(&self) {
        if self.halted.replace(false) {
            tracing::info!("terminal: restarting the shell on request");
            self.guard.borrow_mut().reset();
            self.spawn_now();
        }
    }

    /// How names have to be quoted for the shell that runs in this terminal.
    pub(crate) fn quote_style(&self) -> QuoteStyle {
        self.kind.get().quote_style()
    }

    /// Show the panels again over the terminal.
    pub(crate) fn hide(&self) {
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
        let focus_in_terminal = focus.is_some_and(|f| f.is_ancestor(&self.term));
        self.slot.set_visible(false);
        self.panels.set_visible(true);
        if focus_in_terminal && let Some(ctx) = self.ctx.upgrade() {
            self.lists[ctx.active_side()].grab_focus();
        }
    }

    /// Whether the terminal (not the panels) is currently shown full size.
    pub(crate) fn is_visible(&self) -> bool {
        !self.panels.is_visible()
    }

    /// The command line entry, if T4.2 has built one inside `cmdline_slot` (it sits one or two
    /// levels down, in the prompt row, so the whole subtree is searched).
    fn cmdline_entry(&self) -> Option<gtk::Entry> {
        find_entry(self.cmdline_slot.upcast_ref())
    }
}

/// Colours, font, scrollback and layout of a VTE widget of this window.
fn style_terminal(ctx: &AppContext, term: &vte4::Terminal) {
    {
        let cfg = ctx.config.borrow();
        term.set_scrollback_lines(i64::from(cfg.terminal.scrollback));
        if !cfg.terminal.font.is_empty() {
            term.set_font(Some(&gtk::pango::FontDescription::from_string(
                &cfg.terminal.font,
            )));
        }
    }
    // Theme colours for text/background (the 256-colour palette stays at VTE defaults).
    let palette = ctx.theme.palette();
    term.set_colors(Some(&palette.panel.fg), Some(&palette.panel.bg), &[]);
    term.set_vexpand(true);
    term.set_hexpand(true);
    term.add_css_class("ofm-terminal");
}

/// Keep `AppContext::terminal_focused` in sync with the focus of a VTE widget (key resolution
/// consults only the `global` keymap section while it is set).
fn track_focus(ctx: &Rc<AppContext>, term: &vte4::Terminal) {
    let weak_ctx = Rc::downgrade(ctx);
    let focus = gtk::EventControllerFocus::new();
    focus.connect_enter(move |_| {
        if let Some(ctx) = weak_ctx.upgrade() {
            ctx.set_terminal_focused(true);
        }
    });
    let weak_ctx = Rc::downgrade(ctx);
    focus.connect_leave(move |_| {
        if let Some(ctx) = weak_ctx.upgrade() {
            ctx.set_terminal_focused(false);
        }
    });
    term.add_controller(focus);
}

/// Ctrl+Shift+C / Ctrl+Shift+V on a program's VTE widget (the shell view handles them in
/// `wire_keys`).
fn add_clipboard_keys(term: &vte4::Terminal) {
    let key_ctl = gtk::EventControllerKey::new();
    key_ctl.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let t = term.clone();
    key_ctl.connect_key_pressed(move |_, keyval, keycode, state| {
        use ofm_core::{Key, Mods};
        let Some(chord) = keys::chord_from_event(keyval, keycode, state) else {
            return glib::Propagation::Proceed;
        };
        let Mods {
            ctrl: true,
            shift: true,
            alt: false,
            super_: false,
            ..
        } = chord.mods
        else {
            return glib::Propagation::Proceed;
        };
        let is = |k: char| matches!(chord.key, Key::Char(c) if c.eq_ignore_ascii_case(&k));
        if is('c') {
            t.emit_copy_clipboard();
            glib::Propagation::Stop
        } else if is('v') {
            t.paste_clipboard();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    term.add_controller(key_ctl);
}

/// First `gtk::Entry` in the subtree of `w` (depth first).
fn find_entry(w: &gtk::Widget) -> Option<gtk::Entry> {
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(entry) = c.downcast_ref::<gtk::Entry>() {
            return Some(entry.clone());
        }
        if let Some(found) = find_entry(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// Per-side wiring for shell-follows-panel (the frozen `install_panel_view` hook): tab
/// switches and `PanelEvent::Location` of the active tab trigger a cd sync consideration.
/// Runs before [`TerminalUi`] exists (the views are built first), so the handlers look it
/// up in `ctx.ext` at event time.
pub(crate) fn install_panel_view(ctx: &Rc<AppContext>, side: Side) {
    let host = Rc::clone(ctx.host(side));
    let last: TabWatch = Rc::default();
    bind_active_tab(ctx, &host, &last);
    let weak_ctx = Rc::downgrade(ctx);
    let host2 = Rc::clone(&host);
    host.active_changed.connect(move |ctrl| {
        if let Some(ctx) = weak_ctx.upgrade() {
            bind_active_tab(&ctx, &host2, &last);
            maybe_sync_from_event(&ctx, ctrl);
        }
    });
}

/// The currently subscribed active tab of one side (weak: a closed tab simply disappears).
type TabWatch = Rc<RefCell<Option<(Weak<PanelController>, SignalId)>>>;

/// Subscribe to `Location` events of `host`'s active tab; the subscription follows the
/// active tab (the old one is disconnected, so switching back and forth does not pile up).
fn bind_active_tab(ctx: &Rc<AppContext>, host: &Rc<PanelHost>, last: &TabWatch) {
    let ctrl = host.active();
    if let Some((prev, id)) = last.borrow().as_ref()
        && let Some(prev) = prev.upgrade()
    {
        if Rc::ptr_eq(&prev, &ctrl) {
            return; // still bound
        }
        prev.changed.disconnect(*id);
    }
    let weak_ctx = Rc::downgrade(ctx);
    let weak_ctrl = Rc::downgrade(&ctrl);
    let id = ctrl.changed.connect(move |ev| {
        if matches!(ev, PanelEvent::Location)
            && let (Some(ctx), Some(ctrl)) = (weak_ctx.upgrade(), weak_ctrl.upgrade())
        {
            maybe_sync_from_event(&ctx, &ctrl);
        }
    });
    *last.borrow_mut() = Some((Rc::downgrade(&ctrl), id));
}

fn maybe_sync_from_event(ctx: &Rc<AppContext>, ctrl: &Rc<PanelController>) {
    // Only the active tab of the active side drives the shell directory.
    let active = ctx.active_controller();
    if Rc::ptr_eq(ctrl, &active) {
        maybe_sync_from_ctx(&Rc::downgrade(ctx));
    }
}

fn maybe_sync_from_ctx(weak_ctx: &Weak<AppContext>) {
    if let Some(ctx) = weak_ctx.upgrade()
        && let Some(ui) = ctx.ext.get::<TerminalUi>()
    {
        ui.maybe_sync();
    }
}
