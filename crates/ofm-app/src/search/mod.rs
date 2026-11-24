//! Find files (F24) and panelize (F25), T7.3 / ADR-0009.
//!
//! ```text
//! dialogs::find   the Find window (form, results ColumnView, buttons)      GTK widgets
//! search::run     SearchRun: engine events -> 20 Hz UI updates             no widgets
//! search::model   HitModel: gio::ListModel over a Vec<Hit>, lazy rows
//! search::form    dialog fields -> SearchQuery (sizes, dates, masks)       pure
//! search::rows    Hit -> cell texts, go-to / view targets                  pure
//! search::batch   update-rate limiter                                      pure
//! search::lists   ListHub: panelized lists (ListStore + titles), in Services
//! search::panelize / panelize_dialog   results and shell output -> list panel
//! ```
//!
//! Commands: `search.find` (Alt+F7), `search.panelize_command` (Ctrl+Alt+P), and
//! `search.panelize_results` (F10 inside the Find window; a no-op elsewhere). Keys in the Find
//! window go through the central dispatcher like every dialog (`WindowKind::Dialog`); the window is
//! not modal, so [`route_dialog_key`] (called by `dialog.ok` / `dialog.cancel`) decides whether
//! Enter / Esc belong to it. F3 / F4 in the results use the keymap's `panels` bindings of
//! `file.view` / `file.edit` through the dialog key fallback.

pub mod actions;
pub mod batch;
pub mod form;
pub mod lists;
pub mod model;
pub mod panelize;
pub mod panelize_dialog;
pub mod rows;
pub mod run;
pub mod style;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use ofm_core::cmd;

use crate::context::AppContext;
use crate::dialogs::find::FindWindow;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use run::{Starter, real_starter};

/// Enter / Esc, as routed by `dialog.ok` / `dialog.cancel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKey {
    /// `dialog.ok`.
    Ok,
    /// `dialog.cancel`.
    Cancel,
}

/// Module state in `ctx.ext`.
pub struct FindState {
    starter: RefCell<Starter>,
    window: RefCell<Option<Rc<FindWindow>>>,
    last_message: RefCell<Option<String>>,
}

impl FindState {
    fn new() -> Self {
        FindState {
            starter: RefCell::new(real_starter()),
            window: RefCell::new(None),
            last_message: RefCell::new(None),
        }
    }

    /// The open Find window, if any.
    pub fn window(&self) -> Option<Rc<FindWindow>> {
        self.window.borrow().clone()
    }

    pub(crate) fn set_window(&self, w: Option<Rc<FindWindow>>) {
        *self.window.borrow_mut() = w;
    }

    /// The engine used to start searches.
    pub fn starter(&self) -> Starter {
        self.starter.borrow().clone()
    }

    /// Replace the engine (tests inject a fake).
    pub fn set_starter(&self, starter: Starter) {
        *self.starter.borrow_mut() = starter;
    }

    /// The last message shown by [`notify`] (tests).
    pub fn last_message(&self) -> Option<String> {
        self.last_message.borrow().clone()
    }
}

/// The state of `ctx`.
pub fn state(ctx: &AppContext) -> Option<Rc<FindState>> {
    ctx.ext.get::<FindState>()
}

/// Show a one-line message above the F-key bar (and remember it for tests).
pub fn notify(ctx: &Rc<AppContext>, text: &str) {
    if let Some(s) = state(ctx) {
        *s.last_message.borrow_mut() = Some(text.to_owned());
    }
    crate::compare::message::show(ctx, text);
}

/// Register the three commands.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::SEARCH_FIND, |ctx| {
        crate::dialogs::find::open(ctx);
    });
    reg.register(cmd::SEARCH_PANELIZE_COMMAND, panelize_dialog::open);
    reg.register(cmd::SEARCH_PANELIZE_RESULTS, |ctx| {
        // F10 is bound in the `dialog` section: only act when the Find window has the focus.
        if let Some(w) = state(ctx).and_then(|s| s.window())
            && w.window.is_active()
        {
            w.panelize();
        }
    });
}

/// Watch the tabs of one panel side: when a list panel finishes a refresh with fewer rows than it
/// had (the re-stat dropped vanished files), say how many (`ListVfs` only logs it).
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &crate::window::PanelView) {
    use std::cell::RefCell;
    use std::rc::Weak;

    use crate::panel::{LoadState, PanelController, PanelEvent};

    let host = Rc::clone(ctx.host(view.side));
    let watched: Rc<RefCell<Vec<Weak<PanelController>>>> = Rc::default();
    let attach = {
        let (weak_ctx, watched) = (Rc::downgrade(ctx), Rc::clone(&watched));
        move |host: &crate::panel::PanelHost| {
            for tab in host.tabs() {
                if watched
                    .borrow()
                    .iter()
                    .any(|w| w.as_ptr() == Rc::as_ptr(&tab))
                {
                    continue;
                }
                watched.borrow_mut().push(Rc::downgrade(&tab));
                let (weak_ctx, weak_tab) = (weak_ctx.clone(), Rc::downgrade(&tab));
                tab.changed.connect(move |ev| {
                    if *ev != PanelEvent::State {
                        return;
                    }
                    if let (Some(ctx), Some(tab)) = (weak_ctx.upgrade(), weak_tab.upgrade())
                        && tab.state() == LoadState::Idle
                    {
                        check_vanished(&ctx, &tab);
                    }
                });
            }
        }
    };
    attach(&host);
    let host2 = Rc::clone(&host);
    host.tabs_changed.connect(move |()| attach(&host2));
}

/// Compare the row count of a refreshed list panel with what it should have.
fn check_vanished(ctx: &Rc<AppContext>, tab: &Rc<crate::panel::PanelController>) {
    let ofm_core::Location::List { id } = tab.location() else {
        return;
    };
    let hub = &ctx.services.lists;
    let (Some(expected), actual) = (hub.expected(id), tab.model().total_len()) else {
        return;
    };
    if actual < expected {
        hub.set_expected(id, actual);
        notify(
            ctx,
            &format!(
                "{} {}",
                expected - actual,
                tr("items no longer exist and were removed from the list.")
            ),
        );
    }
}

/// Create the module state.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(FindState::new()));
}

/// `dialog.ok` / `dialog.cancel` came in: if the Find window is the focused window it takes the
/// key (returns true), otherwise the modal dialog stack handles it.
pub fn route_dialog_key(ctx: &Rc<AppContext>, key: DialogKey) -> bool {
    let Some(w) = state(ctx).and_then(|s| s.window()) else {
        return false;
    };
    if !w.window.is_active() {
        return false;
    }
    match key {
        DialogKey::Ok => w.on_ok(),
        DialogKey::Cancel => w.on_cancel(),
    }
    true
}
