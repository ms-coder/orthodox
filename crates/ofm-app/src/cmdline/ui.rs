//! The command line widgets and command bodies (F11, ADR-0003). Pure decisions live in the
//! sibling modules ([`super::exec`], [`super::complete`], …); this file only wires them to GTK.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::ops::ControlFlow;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use ofm_core::Location;
use ofm_core::cmdline::{QuoteStyle, complete_in, shell_quote_for};

use super::complete;
use super::exec;
use super::prompt;
use super::state;
use super::typing;
use crate::context::AppContext;
use crate::i18n::tr;
use crate::keys;
use crate::navigation::chooser::{self, ChoiceRow, ChooserSpec};
use crate::navigation::goto;
use crate::navigation::store::HistoryStore;
use crate::panel::{LEFT, PanelController, PanelEvent};
use crate::terminal::{self, ShellState};
use crate::window::{MainWindow, PanelView};
use crate::wiring;

/// Text of the busy marker (shown in the prompt row while the shell runs a command).
const BUSY_MARK: &str = "…";

/// The command line widgets, built once by [`install`] (state in `ctx.ext`).
pub(crate) struct Widgets {
    /// Prompt label (active panel's directory, `~/x$ `).
    pub(crate) prompt: gtk::Label,
    /// The entry itself.
    pub(crate) entry: gtk::Entry,
    /// Subtle busy marker at the row's end while the shell is [`ShellState::Busy`].
    busy: gtk::Label,
    /// Candidate list for ambiguous completions.
    popover: gtk::Popover,
    list: gtk::ListBox,
    /// Names behind the popover rows (index = row index).
    candidates: RefCell<Vec<(OsString, bool)>>,
}

/// The window's widgets, if [`install`] already ran.
fn widgets(ctx: &AppContext) -> Option<Rc<Widgets>> {
    state(ctx).and_then(|st| st.widgets.borrow().clone())
}

/// Build the row into `win.cmdline_slot`, load the history, install the busy hint and the
/// `command_line` typing-mode key routing.
pub(crate) fn install(ctx: &Rc<AppContext>, win: &MainWindow) {
    let Some(st) = state(ctx) else { return };

    // Load the persisted command history once (the store's first use reads history.toml — the
    // documented start-up exception to the no-I/O-on-the-main-thread rule).
    let store = HistoryStore::ensure(ctx);
    {
        let mut ring = st.ring.borrow_mut();
        for line in store.snapshot().commands.iter().rev() {
            ring.push(line); // oldest first, so the newest lands at the front
        }
    }
    let _ = st.store.set(store);

    // Row: prompt + entry (+ busy marker). Monospace font and colours come from the
    // `.ofm-cmdline` class chrome put on the slot (theme CSS).
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.set_margin_start(4);
    row.set_margin_end(4);
    let prompt = gtk::Label::new(None);
    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    entry.update_property(&[gtk::accessible::Property::Label(tr("Command line"))]);
    let busy = gtk::Label::new(None);
    busy.add_css_class("dim-label");
    row.append(&prompt);
    row.append(&entry);
    row.append(&busy);
    win.cmdline_slot.append(&row);

    // Candidate popover for ambiguous completions, above the entry (shell behaviour).
    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Top);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.update_property(&[gtk::accessible::Property::Label(tr("Completions"))]);
    popover.set_child(Some(&list));
    popover.set_parent(&entry);

    let w = Rc::new(Widgets {
        prompt,
        entry,
        busy,
        popover,
        list,
        candidates: RefCell::new(Vec::new()),
    });

    // Picking a candidate completes the word (mouse; rows are not keyboard-focusable, the
    // entry keeps the focus).
    {
        let w_weak = Rc::downgrade(&w);
        let ctx_weak = Rc::downgrade(ctx);
        w.list.connect_row_activated(move |_, row| {
            let Some(w) = w_weak.upgrade() else { return };
            let style = ctx_weak
                .upgrade()
                .map(|ctx| quote_style(&ctx))
                .unwrap_or_default();
            let idx = usize::try_from(row.index()).unwrap_or(0);
            let Some((name, is_dir)) = w.candidates.borrow().get(idx).cloned() else {
                return;
            };
            let text = w.entry.text().to_string();
            let cursor = usize::try_from(w.entry.position()).unwrap_or(0);
            let word = complete::split_word_before(&text, cursor);
            let word_start = cursor - word.dir.len() - word.prefix.len();
            w.popover.popdown();
            replace_word(
                &w.entry,
                word_start,
                cursor,
                &complete::completed_word(&name, is_dir, style),
            );
            w.entry.grab_focus();
        });
    }

    // While the candidate list is open the popover holds the keyboard grab, so keys land in
    // the popover and GTK does not forward unhandled ones to the entry: forward the editing
    // keys and run the line commands explicitly, so an open candidate list never swallows
    // input (typing continues the completion, Enter/Esc/Tab behave as with the list closed).
    {
        let weak_ctx = Rc::downgrade(ctx);
        let w_weak = Rc::downgrade(&w);
        let controller = gtk::EventControllerKey::new();
        controller.connect_key_pressed(move |_c, keyval, _code, state| {
            let (Some(ctx), Some(w)) = (weak_ctx.upgrade(), w_weak.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            if !w.popover.is_visible() {
                return glib::Propagation::Proceed;
            }
            let stop = glib::Propagation::Stop;
            if keyval == gtk::gdk::Key::Escape {
                w.popover.popdown();
                clear(&ctx); // Esc with text clears the line (cmdline.clear)
                return stop;
            }
            if matches!(keyval, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
                w.popover.popdown();
                execute(&ctx);
                return stop;
            }
            if keyval == gtk::gdk::Key::Tab
                || (keyval == gtk::gdk::Key::space
                    && state.contains(gtk::gdk::ModifierType::CONTROL_MASK))
            {
                w.popover.popdown();
                complete(&ctx);
                return stop;
            }
            if keyval == gtk::gdk::Key::BackSpace {
                let pos = w.entry.position().max(0);
                if pos > 0 {
                    w.entry.delete_text(pos - 1, pos);
                }
                return stop;
            }
            if let Some(c) = keyval.to_unicode()
                && !c.is_control()
                && !state.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK
                        | gtk::gdk::ModifierType::ALT_MASK
                        | gtk::gdk::ModifierType::SUPER_MASK,
                )
            {
                insert_at_cursor(&ctx, &c.to_string());
                return stop;
            }
            glib::Propagation::Proceed
        });
        w.popover.add_controller(controller);
    }

    // Every text change updates the flag that switches key resolution to the [cmdline] section,
    // invalidates in-flight completions and closes a candidate list.
    {
        let weak = Rc::downgrade(ctx);
        w.entry.connect_changed(move |e| {
            let Some(ctx) = weak.upgrade() else { return };
            ctx.set_cmdline_has_text(!e.text().is_empty());
            if let Some(st) = state(&ctx)
                && let Some(w) = st.widgets.borrow().as_ref()
            {
                st.generation.set(st.generation.get() + 1);
                if w.popover.is_visible() {
                    w.popover.popdown();
                }
            }
        });
    }

    // Busy hint: while the shell runs a command a subtle marker shows at the row's end.
    if let Some(host) = terminal::shell_host(ctx) {
        let label = w.busy.clone();
        host.changed.connect(move |s| {
            label.set_text(if *s == ShellState::Busy {
                BUSY_MARK
            } else {
                ""
            });
        });
        w.busy.set_text(if host.get() == ShellState::Busy {
            BUSY_MARK
        } else {
            ""
        });
    }

    // `command_line` typing mode (ADR-0005): plain printable keys that no command claimed focus
    // the entry and are typed there. Bubble phase, added after quicksearch's controller (hooks
    // order), which claims only Alt+letter in this mode; text entries and modal dialogs win.
    {
        let weak = Rc::downgrade(ctx);
        let window = win.window.clone();
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Bubble);
        controller.connect_key_pressed(move |_c, keyval, _keycode, state| {
            let Some(ctx) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let focus_is_text = gtk::prelude::GtkWindowExt::focus(&window)
                .is_some_and(|f| f.is::<gtk::Editable>() || f.is::<gtk::TextView>());
            if ctx.modal_open() || ctx.terminal_focused() || focus_is_text {
                return glib::Propagation::Proceed;
            }
            let Some(chord) = keys::chord_from_key(keyval, state) else {
                return glib::Propagation::Proceed;
            };
            let typing = ctx.keymap.borrow().typing;
            match typing::typed_char_cmdline(typing, &chord, keyval.to_unicode()) {
                Some(c) => {
                    focus_and_type(&ctx, c);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        win.window.add_controller(controller);
    }

    *st.widgets.borrow_mut() = Some(Rc::clone(&w));

    // The prompt follows the active panel: switches between left and right.
    {
        let weak = Rc::downgrade(ctx);
        ctx.active_changed.connect(move |side| {
            if let Some(ctx) = weak.upgrade() {
                reconnect_location(&ctx, ctx.host(*side).active());
                refresh_prompt(&ctx);
            }
        });
    }
    reconnect_location(ctx, ctx.host(ctx.active_side()).active());
    refresh_prompt(ctx);
}

/// Per-panel wiring: tab switches move the Location subscription to the new active tab.
pub(crate) fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    let weak = Rc::downgrade(ctx);
    ctx.host(view.side).active_changed.connect(move |ctrl| {
        if let Some(ctx) = weak.upgrade() {
            reconnect_location(&ctx, Rc::clone(ctrl));
            refresh_prompt(&ctx);
        }
    });
}

/// Point the prompt's Location subscription at `ctrl` (the active tab), dropping the previous
/// one. Non-active tabs navigating in the background do not move the prompt.
fn reconnect_location(ctx: &Rc<AppContext>, ctrl: Rc<PanelController>) {
    let Some(st) = state(ctx) else { return };
    if let Some((weak_ctrl, id)) = st.loc_conn.borrow_mut().take()
        && let Some(old) = weak_ctrl.upgrade()
    {
        old.changed.disconnect(id);
    }
    let id = ctrl.changed.connect({
        let weak = Rc::downgrade(ctx);
        move |ev| {
            if matches!(ev, PanelEvent::Location)
                && let Some(ctx) = weak.upgrade()
            {
                refresh_prompt(&ctx);
            }
        }
    });
    *st.loc_conn.borrow_mut() = Some((Rc::downgrade(&ctrl), id));
}

/// Recompute the prompt from the active panel's location.
fn refresh_prompt(ctx: &AppContext) {
    if let Some(w) = widgets(ctx) {
        let text = prompt::prompt_text(&ctx.active_controller().location(), &glib::home_dir());
        w.prompt.set_text(&text);
    }
}

/// Focus the entry and type `c` at its end (a key that started the `command_line` typing).
fn focus_and_type(ctx: &Rc<AppContext>, c: char) {
    let Some(w) = widgets(ctx) else { return };
    let entry = &w.entry;
    entry.grab_focus();
    let len = entry.text().len() as i32;
    entry.set_position(len);
    let mut pos = len;
    entry.insert_text(&c.to_string(), &mut pos);
    entry.set_position(pos);
}

/// Replace the entry's text and put the cursor at its end; focuses the entry when `focus`.
fn set_entry_text(entry: &gtk::Entry, text: &str, focus: bool) {
    entry.set_text(text);
    entry.set_position(text.len() as i32);
    if focus {
        entry.grab_focus();
    }
}

/// Insert `text` at the entry's cursor (append at the end when the entry was not focused).
fn insert_at_cursor(ctx: &Rc<AppContext>, text: &str) {
    let Some(w) = widgets(ctx) else { return };
    let entry = &w.entry;
    let was_focused = entry.has_focus();
    entry.grab_focus();
    if !was_focused {
        entry.set_position(entry.text().len() as i32);
    }
    let mut pos = entry.position();
    entry.insert_text(text, &mut pos);
    entry.set_position(pos);
}

/// How inserted names are quoted: for the shell that actually runs in the terminal (fish is
/// not POSIX), POSIX while there is no terminal.
fn quote_style(ctx: &AppContext) -> QuoteStyle {
    terminal::handle(ctx)
        .map(|h| h.quote_style())
        .unwrap_or_default()
}

/// Replace `entry[start..end]` with `new_word` and put the cursor after it.
fn replace_word(entry: &gtk::Entry, start: usize, end: usize, new_word: &str) {
    let text = entry.text().to_string();
    let start = start.min(text.len());
    let end = end.clamp(start, text.len());
    let composed = format!("{}{}{}", &text[..start], new_word, &text[end..]);
    entry.set_text(&composed);
    entry.set_position((start + new_word.len()) as i32);
}

/// Give the keyboard focus back to the active panel's file list.
fn focus_active_panel(ctx: &AppContext) {
    if let Some(win) = ctx.window() {
        let list = if ctx.active_side() == LEFT {
            &win.left.list
        } else {
            &win.right.list
        };
        list.grab_focus();
    }
}

// ---- command bodies ------------------------------------------------------------------------------

/// `cmdline.execute` (Enter): a plain resolvable `cd` navigates the active panel; everything
/// else goes to the shell and reveals the terminal. The line lands in the history and the entry
/// is cleared.
pub(crate) fn execute(ctx: &Rc<AppContext>) {
    let Some(w) = widgets(ctx) else { return };
    let text = w.entry.text().to_string();
    if text.trim().is_empty() {
        return; // Enter never reaches the [cmdline] section with an empty line, but be safe.
    }
    let ctrl = ctx.active_controller();
    match exec::decide_execute(&text, &ctrl.location(), &glib::home_dir()) {
        exec::ExecuteAction::Navigate(target) => {
            // No shell round-trip: the next panel→shell sync aligns it (ADR-0003).
            goto::navigate(ctx, &ctrl, Location::Local(target), None);
        }
        exec::ExecuteAction::SendShell => {
            if let Some(handle) = terminal::handle(ctx) {
                // The entry holds UTF-8; inserted non-UTF-8 names sit in ANSI-C `$'…'` escapes
                // that the shell expands back to the exact bytes (see insert_name).
                handle.send_line(text.as_bytes());
                handle.show();
            } else {
                tracing::warn!("cmdline.execute: no terminal handle installed");
            }
        }
    }
    if let Some(st) = state(ctx) {
        st.nav.borrow_mut().reset();
        // A line starting with a space is kept out of the history (like `ignorespace`).
        if exec::records_history(&text) {
            st.ring.borrow_mut().push(&text);
            if let Some(store) = st.store.get() {
                store.update(|h| ofm_config::history::push_recent(&mut h.commands, &text));
            }
        }
    }
    w.entry.set_text("");
}

/// `cmdline.clear` (Esc): clear the text; an already-empty line gives the focus back to the
/// active panel.
pub(crate) fn clear(ctx: &Rc<AppContext>) {
    let Some(w) = widgets(ctx) else { return };
    if w.entry.text().is_empty() {
        focus_active_panel(ctx);
    } else {
        w.entry.set_text("");
    }
}

/// `cmdline.history_prev` (Ctrl+E): step to the previous (older) command; the first step saves
/// the current text as the draft.
pub(crate) fn history_prev(ctx: &Rc<AppContext>) {
    let (Some(st), Some(w)) = (state(ctx), widgets(ctx)) else {
        return;
    };
    let items: Vec<String> = st.ring.borrow().iter().map(str::to_owned).collect();
    let current = w.entry.text().to_string();
    if let Some(text) = st.nav.borrow_mut().step_prev(&items, &current) {
        set_entry_text(&w.entry, &text, true);
    }
}

/// `cmdline.history_next` (Ctrl+Shift+E): step back; past the newest entry the draft returns.
pub(crate) fn history_next(ctx: &Rc<AppContext>) {
    let (Some(st), Some(w)) = (state(ctx), widgets(ctx)) else {
        return;
    };
    let items: Vec<String> = st.ring.borrow().iter().map(str::to_owned).collect();
    if let Some(text) = st.nav.borrow_mut().step_next(&items) {
        set_entry_text(&w.entry, &text, true);
    }
}

/// `cmdline.history_list` (Alt+F8): pick a previous command from the shared chooser dialog.
pub(crate) fn history_list(ctx: &Rc<AppContext>) {
    let Some(st) = state(ctx) else { return };
    let titles: Vec<String> = st.ring.borrow().iter().map(str::to_owned).collect();
    let on_pick = {
        let titles = titles.clone();
        Rc::new(move |ctx: &Rc<AppContext>, row: usize| {
            let (Some(st), Some(w)) = (state(ctx), widgets(ctx)) else {
                return;
            };
            if let Some(text) = titles.get(row) {
                set_entry_text(&w.entry, text, true);
                st.nav.borrow_mut().jump(row);
            }
        })
    };
    chooser::open(
        ctx,
        ChooserSpec {
            title: "Command history",
            rows: titles
                .into_iter()
                .map(|title| ChoiceRow {
                    title,
                    subtitle: None,
                })
                .collect(),
            selected: 0,
            empty_text: "No history yet",
            on_pick,
            buttons: Vec::new(),
        },
    );
}

/// `cmdline.insert_name` (Ctrl+Enter): the name under the panel cursor, quoted when needed.
pub(crate) fn insert_name(ctx: &Rc<AppContext>) {
    let ctrl = ctx.active_controller();
    let Some(e) = ctrl.model().cursor_entry().cloned() else {
        return;
    };
    insert_at_cursor(
        ctx,
        &complete::completed_word(&e.name, e.kind.is_dir_like(), quote_style(ctx)),
    );
}

/// `cmdline.insert_fullpath` (Ctrl+F): the full path of the entry under the panel cursor.
pub(crate) fn insert_fullpath(ctx: &Rc<AppContext>) {
    let ctrl = ctx.active_controller();
    let cursor = ctrl.model().cursor_entry().cloned();
    let path = match (&ctrl.location(), cursor.as_ref()) {
        (Location::Local(dir), Some(e)) => Some(dir.join(&e.name)),
        (Location::List { .. }, Some(e)) => e.real_path.clone(),
        _ => None,
    };
    match path {
        Some(p) => insert_at_cursor(ctx, &shell_quote_for(quote_style(ctx), p.as_os_str())),
        None => tracing::debug!("cmdline.insert_fullpath: no local path under the cursor"),
    }
}

/// `cmdline.insert_left_path` / `cmdline.insert_right_path` (Ctrl+[ / Ctrl+]): the directory
/// shown by that panel.
pub(crate) fn insert_panel_path(ctx: &Rc<AppContext>, side: usize) {
    let loc = ctx.host(side).active().location();
    match loc {
        Location::Local(p) => {
            insert_at_cursor(ctx, &shell_quote_for(quote_style(ctx), p.as_os_str()));
        }
        _ => tracing::debug!(side, "cmdline: panel is not on the local file system"),
    }
}

/// `cmdline.complete` (Ctrl+Space, Tab): complete the word before the cursor against the
/// directory listing fetched **on the I/O pool** (invariant 2 — it may be any directory, not
/// just the panel's).
pub(crate) fn complete(ctx: &Rc<AppContext>) {
    let Some(w) = widgets(ctx) else { return };
    let Some(st) = state(ctx) else { return };
    let text = w.entry.text().to_string();
    let cursor = usize::try_from(w.entry.position()).unwrap_or(0);
    let word = complete::split_word_before(&text, cursor);
    let word_start = cursor - word.dir.len() - word.prefix.len();
    let dir = complete::completion_dir(
        word.dir,
        &ctx.active_controller().location(),
        &glib::home_dir(),
    );
    let prefix = OsString::from(word.prefix);
    let generation = st.generation.get() + 1;
    st.generation.set(generation);

    let vfs = ctx.services.vfs.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let mut names: Vec<(OsString, bool)> = Vec::new();
        // The listing runs on the I/O pool through the VFS (invariant 2) — never std::fs here.
        let loc = Location::Local(dir);
        if let Ok(vfs) = vfs.get(&loc) {
            vfs.list(&loc, &ofm_core::CancelToken::new(), &mut |ev| {
                if let ofm_vfs::ListEvent::Batch(batch) = ev {
                    names.extend(batch.into_iter().map(|e| (e.name, e.kind.is_dir_like())));
                }
            });
        }
        names.sort();
        let _ = tx.send_blocking((generation, word_start, cursor, prefix, names));
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |(generation, word_start, cursor, prefix, names)| {
            if let Some(ctx) = weak.upgrade() {
                apply_completion(&ctx, generation, word_start, cursor, &prefix, names);
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

/// Apply a pool-fetched candidate list (stale results are dropped via the generation counter).
fn apply_completion(
    ctx: &Rc<AppContext>,
    generation: u64,
    word_start: usize,
    cursor: usize,
    prefix: &OsStr,
    names: Vec<(OsString, bool)>,
) {
    let (Some(st), Some(w)) = (state(ctx), widgets(ctx)) else {
        return;
    };
    if st.generation.get() != generation {
        return; // the line changed meanwhile
    }
    let prefix_bytes = prefix.as_encoded_bytes();
    let matched: Vec<(OsString, bool)> = names
        .into_iter()
        .filter(|(n, _)| n.as_encoded_bytes().starts_with(prefix_bytes))
        .collect();
    if matched.is_empty() {
        tracing::debug!(prefix = ?prefix, "cmdline.complete: no matches");
        return;
    }
    let candidates: Vec<OsString> = matched.iter().map(|(n, _)| n.clone()).collect();
    let comp = complete_in(&candidates, prefix);
    if comp.unique {
        let (name, is_dir) = &matched[0];
        replace_word(
            &w.entry,
            word_start,
            cursor,
            &complete::completed_word(name, *is_dir, quote_style(ctx)),
        );
        return;
    }
    // Ambiguous: extend by the common prefix (when it is typable) and list the candidates.
    if let Some(ext) = complete::extend_typable(&comp.extend) {
        let mut word_text = prefix.to_string_lossy().into_owned();
        word_text.push_str(&ext);
        replace_word(&w.entry, word_start, cursor, &word_text);
    }
    show_candidates(&w, matched);
}

/// Fill and show the candidate popover (ambiguous completions).
fn show_candidates(w: &Rc<Widgets>, matched: Vec<(OsString, bool)>) {
    while let Some(child) = w.list.first_child() {
        w.list.remove(&child);
    }
    for (name, is_dir) in &matched {
        let mut display = name.to_string_lossy().into_owned();
        if *is_dir {
            display.push('/');
        }
        let label = gtk::Label::new(Some(&display));
        label.set_xalign(0.0);
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&label));
        row.set_can_focus(false); // the entry keeps the keyboard focus
        w.list.append(&row);
    }
    *w.candidates.borrow_mut() = matched;
    w.popover.popup();
}
