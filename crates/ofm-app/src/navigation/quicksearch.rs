//! Quick search and filter-as-you-type (F06).
//!
//! A small bar (entry) overlays the bottom-left of the panel. Quick search jumps the cursor to the
//! first match while typing (prefix before substring, see `PanelModel::quick_search`); filter mode
//! hides non-matching rows instead. Typing mode comes from the keymap (`typing`): in `quick_search`
//! mode plain letters open the bar, in `command_line` mode Alt+letter does.
//!
//! Keys while the bar is open: Up/Down move the cursor as usual, Ctrl+Up/Down jump to the
//! previous/next match, Enter (`panel.open`) closes the bar and enters the entry (a filter stays),
//! Esc (`panel.cancel`) closes the bar and drops the filter.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use ofm_config::TypingMode;
use ofm_core::cmd;
use ofm_core::panel::Direction;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::keys;
use crate::panel::Side;
use crate::widgets::file_list::FileList;
use crate::window::{MainWindow, PanelView};

/// What the bar does with its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Move the cursor to the first match.
    Search,
    /// Hide rows that do not match.
    Filter,
}

/// The bar of one side.
pub struct SearchBar {
    side: Side,
    ctx: Weak<AppContext>,
    root: gtk::Box,
    caption: gtk::Label,
    entry: gtk::Entry,
    list: FileList,
    mode: Cell<Mode>,
    open: Cell<bool>,
    /// True while the text is changed by the code itself.
    quiet: Cell<bool>,
    last_query: RefCell<String>,
}

/// Bars of both sides (state in `ctx.ext`).
#[derive(Default)]
pub struct QuickSearchState {
    bars: [RefCell<Option<Rc<SearchBar>>>; 2],
}

impl QuickSearchState {
    /// Bar of `side`, once its panel view exists.
    pub fn bar(&self, side: Side) -> Option<Rc<SearchBar>> {
        self.bars[side.min(1)].borrow().clone()
    }
}

/// Register the commands of this module.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_QUICKSEARCH, |ctx| {
        open_active(ctx, Mode::Search, "")
    });
    reg.register(cmd::PANEL_FILTER, |ctx| {
        let current = ctx
            .active_controller()
            .model()
            .filter()
            .unwrap_or("")
            .to_owned();
        open_active(ctx, Mode::Filter, &current);
    });
    reg.register(cmd::PANEL_QUICKSEARCH_NEXT, |ctx| {
        step(ctx, Direction::Forward)
    });
    reg.register(cmd::PANEL_QUICKSEARCH_PREV, |ctx| {
        step(ctx, Direction::Backward)
    });
    reg.register(cmd::PANEL_CANCEL, cancel);
}

/// Create the (initially empty) state.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(QuickSearchState::default()));
}

/// Create the bar of `view` on its overlay and register it.
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    let Some(state) = ctx.ext.get::<QuickSearchState>() else {
        return;
    };
    let bar = SearchBar::new(ctx, view);
    *state.bars[view.side.min(1)].borrow_mut() = Some(Rc::clone(&bar));
    // Leaving the tab or the panel closes the bar (a filter stays on its tab).
    ctx.host(view.side).active_changed.connect({
        let bar = Rc::downgrade(&bar);
        move |_| {
            if let Some(b) = bar.upgrade() {
                b.close(true);
            }
        }
    });
    ctx.active_changed.connect({
        let bar = Rc::downgrade(&bar);
        let side = view.side;
        move |active| {
            if *active != side
                && let Some(b) = bar.upgrade()
            {
                b.close(true);
            }
        }
    });
}

/// Window-level typing: plain letters (`quick_search` typing mode) or Alt+letter (`command_line`
/// mode) that no command claimed start a quick search. Bubble phase, so commands and focused
/// text entries always win.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    let weak = Rc::downgrade(ctx);
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let window = win.window.clone();
    controller.connect_key_pressed(move |_c, keyval, _keycode, state| {
        let Some(ctx) = weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        let focus_is_text = gtk::prelude::GtkWindowExt::focus(&window)
            .is_some_and(|f| f.is::<gtk::Editable>() || f.is::<gtk::TextView>());
        if ctx.modal_open() || focus_is_text {
            return gtk::glib::Propagation::Proceed;
        }
        let Some(chord) = keys::chord_from_key(keyval, state) else {
            return gtk::glib::Propagation::Proceed;
        };
        let typing = ctx.keymap.borrow().typing;
        match typed_char(typing, &chord, keyval.to_unicode()) {
            Some(c) => {
                start_with(&ctx, c);
                gtk::glib::Propagation::Stop
            }
            None => gtk::glib::Propagation::Proceed,
        }
    });
    win.window.add_controller(controller);
}

/// The character a key press starts a quick search with, if any (pure; unit-tested).
pub fn typed_char(
    typing: TypingMode,
    chord: &ofm_core::KeyChord,
    unicode: Option<char>,
) -> Option<char> {
    let c = unicode.filter(|c| !c.is_control() && !c.is_whitespace())?;
    let m = chord.mods;
    if m.ctrl || m.super_ {
        return None;
    }
    match typing {
        TypingMode::QuickSearch if !m.alt => Some(c),
        TypingMode::CommandLine if m.alt => Some(c),
        _ => None,
    }
}

/// Open quick search on the active panel with `c` already typed (what a key press does).
/// `[panels] filter_as_you_type = true` turns typing into filter mode instead (F06).
pub fn start_with(ctx: &Rc<AppContext>, c: char) {
    let mode = if ctx.config.borrow().panels.filter_as_you_type {
        Mode::Filter
    } else {
        Mode::Search
    };
    open_active(ctx, mode, &c.to_string());
}

/// Bar of the active side.
pub fn active_bar(ctx: &AppContext) -> Option<Rc<SearchBar>> {
    ctx.ext
        .get::<QuickSearchState>()
        .and_then(|s| s.bar(ctx.active_side()))
}

/// Enter pressed while the bar is open: close it, keeping a filter.
pub fn confirm_active(ctx: &AppContext) {
    if let Some(bar) = active_bar(ctx)
        && bar.is_open()
    {
        bar.close(true);
    }
}

fn open_active(ctx: &Rc<AppContext>, mode: Mode, initial: &str) {
    if let Some(bar) = active_bar(ctx) {
        bar.open(mode, initial);
    }
}

fn step(ctx: &Rc<AppContext>, dir: Direction) {
    let Some(bar) = active_bar(ctx) else { return };
    let query = bar.last_query();
    if query.is_empty() {
        return;
    }
    ctx.active_controller().mutate(|m| {
        m.quick_search_step(&query, dir);
    });
}

/// Esc: close the bar and drop the filter; else drop a lingering filter; else stop a listing.
fn cancel(ctx: &Rc<AppContext>) {
    if crate::compare::content::cancel_running(ctx) {
        return; // T8.1c: Esc stops a running content comparison first
    }
    if let Some(bar) = active_bar(ctx)
        && bar.is_open()
    {
        bar.close(false);
        return;
    }
    let ctrl = ctx.active_controller();
    if ctrl.model().filter().is_some() {
        ctrl.mutate_view(|m| m.set_filter(None));
    } else if ctrl.state() == crate::panel::LoadState::Loading {
        ctrl.cancel_load();
    }
}

impl SearchBar {
    fn new(ctx: &Rc<AppContext>, view: &PanelView) -> Rc<SearchBar> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        root.add_css_class("ofm-quicksearch");
        root.set_halign(gtk::Align::Start);
        root.set_valign(gtk::Align::End);
        root.set_margin_start(8);
        root.set_margin_bottom(8);
        root.set_visible(false);
        let caption = gtk::Label::new(Some(tr("Search:")));
        let entry = gtk::Entry::new();
        entry.set_width_chars(22);
        entry.update_property(&[gtk::accessible::Property::Label(tr("Quick search"))]);
        root.append(&caption);
        root.append(&entry);
        view.overlay.add_overlay(&root);
        let bar = Rc::new(SearchBar {
            side: view.side,
            ctx: Rc::downgrade(ctx),
            root,
            caption,
            entry,
            list: view.list.clone(),
            mode: Cell::new(Mode::Search),
            open: Cell::new(false),
            quiet: Cell::new(false),
            last_query: RefCell::new(String::new()),
        });
        let weak = Rc::downgrade(&bar);
        bar.entry.connect_changed(move |_| {
            if let Some(b) = weak.upgrade() {
                b.apply();
            }
        });
        bar
    }

    /// Whether the bar is shown.
    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// Current mode.
    pub fn mode(&self) -> Mode {
        self.mode.get()
    }

    /// Text in the bar.
    pub fn text(&self) -> String {
        self.entry.text().to_string()
    }

    /// The query of the last search (kept after the bar closed, for Ctrl+Up/Down).
    pub fn last_query(&self) -> String {
        self.last_query.borrow().clone()
    }

    /// Show the bar in `mode` with `initial` text and apply it.
    pub fn open(&self, mode: Mode, initial: &str) {
        self.mode.set(mode);
        self.caption.set_text(match mode {
            Mode::Search => tr("Search:"),
            Mode::Filter => tr("Filter:"),
        });
        self.quiet.set(true);
        self.entry.set_text(initial);
        self.quiet.set(false);
        self.open.set(true);
        self.root.set_visible(true);
        self.entry.grab_focus();
        self.entry.set_position(-1); // no select-all: the next key must not replace the text
        self.apply();
    }

    /// Hide the bar. In filter mode the filter is dropped unless `keep_filter`.
    pub fn close(&self, keep_filter: bool) {
        if !self.open.replace(false) {
            return;
        }
        self.quiet.set(true);
        self.entry.set_text("");
        self.quiet.set(false);
        self.root.set_visible(false);
        self.list.grab_focus();
        if self.mode.get() == Mode::Filter
            && !keep_filter
            && let Some(ctx) = self.ctx.upgrade()
        {
            ctx.host(self.side)
                .active()
                .mutate_view(|m| m.set_filter(None));
        }
    }

    fn apply(&self) {
        if self.quiet.get() || !self.open.get() {
            return;
        }
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let text = self.entry.text().to_string();
        let ctrl = ctx.host(self.side).active();
        match self.mode.get() {
            Mode::Search => {
                *self.last_query.borrow_mut() = text.clone();
                if !text.is_empty() {
                    ctrl.mutate(|m| {
                        m.quick_search(&text);
                    });
                }
            }
            Mode::Filter => {
                *self.last_query.borrow_mut() = text.clone();
                ctrl.mutate_view(|m| m.set_filter((!text.is_empty()).then_some(text.as_str())));
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_core::KeyChord;

    use super::*;

    fn chord(s: &str) -> KeyChord {
        s.parse().unwrap()
    }

    #[test]
    fn typing_modes() {
        // TC: plain letters, not Alt/Ctrl.
        assert_eq!(
            typed_char(TypingMode::QuickSearch, &chord("A"), Some('a')),
            Some('a')
        );
        assert_eq!(
            typed_char(TypingMode::QuickSearch, &chord("Alt+A"), Some('a')),
            None
        );
        assert_eq!(
            typed_char(TypingMode::QuickSearch, &chord("Ctrl+A"), Some('a')),
            None
        );
        // Far: Alt+letter only.
        assert_eq!(
            typed_char(TypingMode::CommandLine, &chord("Alt+A"), Some('a')),
            Some('a')
        );
        assert_eq!(
            typed_char(TypingMode::CommandLine, &chord("A"), Some('a')),
            None
        );
        // No character / control / space.
        assert_eq!(
            typed_char(TypingMode::QuickSearch, &chord("Up"), None),
            None
        );
        assert_eq!(
            typed_char(TypingMode::QuickSearch, &chord("Space"), Some(' ')),
            None
        );
    }
}
