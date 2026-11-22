//! Command dispatch (CLAUDE.md invariant 5: every action is a command).
//!
//! Flow: `EventControllerKey` (capture phase on the main window and on every presented dialog,
//! see [`install_key_controller`]) -> [`keys::chord_from_event`] ->
//! `Keymap` lookup in the right context (ADR-0005) -> [`CommandDispatcher::execute`]. Menus, the
//! F-key bar, mouse actions and tests call `AppContext::execute(cmd)` directly.
//!
//! Handlers are registered by each module's `install_commands` hook (see `hooks.rs`); no module
//! edits this file. A command without a handler returns [`ExecResult::NotImplemented`] (and is
//! logged at debug level), so keymap presets can bind everything from day one.

use std::collections::HashMap;
use std::rc::{Rc, Weak};

use gtk::glib;
use gtk::prelude::*;
use ofm_core::{CommandId, KeyChord, KeyContext};

use crate::context::AppContext;
use crate::keys;

/// A command implementation. Runs on the GTK main thread; must not block or do file I/O
/// (start jobs / listings through `PanelController` or `wiring` instead).
pub type Handler = Rc<dyn Fn(&Rc<AppContext>)>;

/// Result of [`CommandDispatcher::execute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecResult {
    /// A handler ran.
    Handled,
    /// The command exists but nobody implemented it yet.
    NotImplemented,
}

/// Command id -> handler table, filled by the `install_commands` hooks.
#[derive(Default)]
pub struct CommandRegistry {
    handlers: HashMap<CommandId, Handler>,
    duplicates: Vec<CommandId>,
}

impl CommandRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the implementation of `id`. A second registration of the same id replaces the
    /// first, is logged as an error and is reported by [`duplicates`](Self::duplicates)
    /// (a test asserts there are none): two tasks must never own one command.
    pub fn register(&mut self, id: CommandId, handler: impl Fn(&Rc<AppContext>) + 'static) {
        if self.handlers.insert(id, Rc::new(handler)).is_some() {
            tracing::error!(command = %id, "command registered twice");
            self.duplicates.push(id);
        }
    }

    /// Whether `id` has a handler.
    pub fn has(&self, id: CommandId) -> bool {
        self.handlers.contains_key(&id)
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// True when nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Ids registered more than once.
    pub fn duplicates(&self) -> &[CommandId] {
        &self.duplicates
    }

    /// Ids without a handler, from the command registry in `ofm-core` (progress overview).
    pub fn unimplemented(&self) -> Vec<CommandId> {
        ofm_core::commands::ALL
            .iter()
            .map(|c| c.id)
            .filter(|id| !self.has(*id))
            .collect()
    }
}

/// Executes commands from a frozen [`CommandRegistry`].
pub struct CommandDispatcher {
    registry: CommandRegistry,
}

impl CommandDispatcher {
    /// Freeze `registry` into a dispatcher.
    pub fn new(registry: CommandRegistry) -> Rc<Self> {
        Rc::new(CommandDispatcher { registry })
    }

    /// The registry (read-only), e.g. to grey out unimplemented menu items.
    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }

    /// Run `cmd` for `ctx`. The handler is cloned out first, so it may call `execute` itself.
    pub fn execute(&self, ctx: &Rc<AppContext>, cmd: CommandId) -> ExecResult {
        // Archive panels are read-only; F5/F6/F3/F4 mean something else there (T6.3).
        if crate::panel::archive::intercept(ctx, cmd) {
            return ExecResult::Handled;
        }
        match self.registry.handlers.get(&cmd).cloned() {
            Some(h) => {
                tracing::debug!(command = %cmd, "execute");
                h(ctx);
                ExecResult::Handled
            }
            None => {
                tracing::debug!(command = %cmd, "command not implemented");
                ExecResult::NotImplemented
            }
        }
    }
}

/// Which window the key controller is attached to; decides the keymap context (ADR-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    /// The main window: `Dialog` while a modal is open, else `CmdLine` (if text) / `Panels` / `Global`.
    Main,
    /// A viewer window: `Viewer` then `Global`.
    Viewer,
    /// A dialog (an `adw::Dialog` hosted in the main window, or a standalone dialog window):
    /// `Dialog` then `Global`.
    Dialog,
}

/// Resolve `chord` to a command for a window of `kind`.
pub fn resolve(ctx: &AppContext, kind: WindowKind, chord: KeyChord) -> Option<CommandId> {
    let keymap = ctx.keymap.borrow();
    match kind {
        WindowKind::Main if ctx.modal_open() => keymap.resolve_in(KeyContext::Dialog, chord),
        // The VTE terminal is a text surface: only `global` chords (Ctrl+O, F10, Ctrl+J, ...).
        WindowKind::Main if ctx.terminal_focused() => keymap.resolve_in(KeyContext::Global, chord),
        WindowKind::Main => keymap.resolve_main(chord, ctx.cmdline_has_text()),
        // A dialog over the viewer window (search, go to, encoding): with an older libadwaita the
        // window's controller sees the dialog's keys first, so resolve them as dialog keys.
        WindowKind::Viewer if ctx.modal_open() => keymap.resolve_in(KeyContext::Dialog, chord),
        WindowKind::Viewer => keymap.resolve_in(KeyContext::Viewer, chord),
        WindowKind::Dialog => keymap.resolve_in(KeyContext::Dialog, chord),
    }
}

/// Text-entry exception (invariant 5): when an editable widget has focus, plain typing and
/// editing keys belong to it and are never looked up. Everything else (F-keys, Esc, Enter,
/// Alt/Ctrl combinations other than the clipboard/select-all set) still reaches the keymap.
pub fn entry_wants_key(chord: &KeyChord) -> bool {
    use ofm_core::Key;
    let m = chord.mods;
    if m.alt || m.super_ {
        return false;
    }
    if m.ctrl {
        return matches!(chord.key, Key::Char('A' | 'C' | 'V' | 'X' | 'Z' | 'Y'))
            || matches!(
                chord.key,
                Key::Left | Key::Right | Key::Backspace | Key::Delete
            );
    }
    matches!(
        chord.key,
        Key::Char(_)
            | Key::Space
            | Key::Backspace
            | Key::Delete
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::Insert
            | Key::Keypad(_)
            | Key::GrayPlus
            | Key::GrayMinus
            | Key::GrayStar
            | Key::GraySlash
    )
}

/// Hook for chords the keymap cannot bind because they are dynamic (the hotkey letters of the
/// user menu entries, F28): while a dialog is open and the `dialog` section has no command for
/// a chord, the dialog that installed a hook may take it. Stored in `ctx.ext`; the hook is
/// cleared when its dialog closes.
#[derive(Default)]
pub struct DialogKeyFallback {
    hook: std::cell::RefCell<Option<DialogKeyHook>>,
}

/// A [`DialogKeyFallback`] hook: returns true when it consumed the chord.
pub type DialogKeyHook = Rc<dyn Fn(&Rc<AppContext>, KeyChord) -> bool>;

impl DialogKeyFallback {
    /// The state in `ctx.ext` (created on first use).
    pub fn ensure(ctx: &AppContext) -> Rc<DialogKeyFallback> {
        ctx.ext.get::<DialogKeyFallback>().unwrap_or_else(|| {
            let fb = Rc::new(DialogKeyFallback::default());
            ctx.ext.insert(Rc::clone(&fb));
            fb
        })
    }

    /// Install `hook` (replaces a previous one).
    pub fn set(&self, hook: DialogKeyHook) {
        *self.hook.borrow_mut() = Some(hook);
    }

    /// Remove the hook.
    pub fn clear(&self) {
        *self.hook.borrow_mut() = None;
    }

    fn offer(ctx: &Rc<AppContext>, chord: KeyChord) -> bool {
        let hook = ctx
            .ext
            .get::<DialogKeyFallback>()
            .and_then(|fb| fb.hook.borrow().clone());
        hook.is_some_and(|h| h(ctx, chord))
    }
}

/// Name of the controller added by [`install_key_controller`] (guards against adding it twice).
const CONTROLLER_NAME: &str = "ofm-dispatch";

/// Whether [`install_key_controller`] already attached the dispatcher to `widget`.
pub fn has_dispatch_controller(widget: &gtk::Widget) -> bool {
    let list = widget.observe_controllers();
    (0..list.n_items()).any(|i| {
        list.item(i)
            .and_downcast::<gtk::EventController>()
            .and_then(|c| c.name())
            .is_some_and(|n| n == CONTROLLER_NAME)
    })
}

/// Attach the capture-phase key controller to `window`. This is the *only* place key events are
/// turned into commands; widgets must not handle keys themselves (except text entries).
///
/// Called for the main window ([`WindowKind::Main`]) and, by `navigation::modal::present`, for
/// every presented `adw::Dialog` ([`WindowKind::Dialog`]): libadwaita (>= 1.7 on GTK >= 4.18)
/// marks a dialog with `gtk_widget_set_limit_events`, so key events targeted inside it are never
/// propagated to the window's controller. With an older libadwaita the window controller sees
/// the event first (capture runs top-down), resolves it in the same `dialog` section and stops it,
/// so the dialog controller never runs: a chord is dispatched exactly once either way.
/// Adding the controller to a widget that already has one is a no-op.
pub fn install_key_controller(
    window: &impl IsA<gtk::Widget>,
    ctx: &Rc<AppContext>,
    kind: WindowKind,
) {
    if has_dispatch_controller(window.as_ref()) {
        return;
    }
    let weak: Weak<AppContext> = Rc::downgrade(ctx);
    let controller = gtk::EventControllerKey::new();
    controller.set_name(Some(CONTROLLER_NAME));
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let widget = window.as_ref().clone();
    controller.connect_key_pressed(move |_c, keyval, keycode, state| {
        let Some(ctx) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let Some(chord) = keys::chord_from_event(keyval, keycode, state) else {
            return glib::Propagation::Proceed;
        };
        let focus_is_text = widget
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|f| f.is::<gtk::Editable>() || f.is::<gtk::TextView>());
        if focus_is_text && entry_wants_key(&chord) {
            return glib::Propagation::Proceed;
        }
        match resolve(&ctx, kind, chord) {
            Some(cmd) => {
                ctx.execute(cmd);
                glib::Propagation::Stop
            }
            None if kind == WindowKind::Dialog
                || (kind == WindowKind::Main && ctx.modal_open()) =>
            {
                if DialogKeyFallback::offer(&ctx, chord) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
            None => glib::Propagation::Proceed,
        }
    });
    window.as_ref().add_controller(controller);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ofm_core::{Key, Mods};

    fn chord(key: Key) -> KeyChord {
        KeyChord {
            key,
            mods: Mods::default(),
        }
    }

    #[test]
    fn registry_detects_duplicates() {
        let mut r = CommandRegistry::new();
        r.register(ofm_core::cmd::PANEL_SWITCH, |_| {});
        assert!(r.has(ofm_core::cmd::PANEL_SWITCH));
        assert!(r.duplicates().is_empty());
        r.register(ofm_core::cmd::PANEL_SWITCH, |_| {});
        assert_eq!(r.duplicates(), &[ofm_core::cmd::PANEL_SWITCH]);
        assert_eq!(r.len(), 1);
        assert!(r.unimplemented().len() > 100);
    }

    #[test]
    fn text_entry_policy() {
        assert!(entry_wants_key(&chord(Key::Char('A'))));
        assert!(entry_wants_key(&chord(Key::Backspace)));
        assert!(!entry_wants_key(&chord(Key::F(5))));
        assert!(!entry_wants_key(&chord(Key::Enter)));
        assert!(!entry_wants_key(&chord(Key::Escape)));
        assert!(!entry_wants_key(&chord(Key::Tab)));
        let alt_a = KeyChord {
            key: Key::Char('A'),
            mods: Mods {
                alt: true,
                ..Mods::default()
            },
        };
        assert!(!entry_wants_key(&alt_a));
    }
}
