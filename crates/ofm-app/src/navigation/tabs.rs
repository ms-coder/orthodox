//! Tabs per panel (F05): the `tab.*` commands and the tab bar widget.
//!
//! A locked tab cannot be closed and never navigates away (see `goto::navigate`: the target opens
//! in a new tab). The bar is hidden while a side has a single unlocked tab.

use std::cell::RefCell;
use std::path::Path;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use ofm_core::{Location, cmd};

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::panel::{PanelController, PanelEvent, PanelHost};
use crate::window::PanelView;

/// Register the `tab.*` commands.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::TAB_NEW, |ctx| {
        let host = ctx.host(ctx.active_side());
        host.add_tab(host.active().location(), true);
    });
    reg.register(cmd::TAB_CLOSE, |ctx| {
        let host = ctx.host(ctx.active_side());
        if !host.close_tab(host.active_index()) {
            tracing::debug!("tab.close refused (last or locked tab)");
        }
    });
    reg.register(cmd::TAB_NEXT, |ctx| step(ctx.host(ctx.active_side()), 1));
    reg.register(cmd::TAB_PREV, |ctx| step(ctx.host(ctx.active_side()), -1));
    reg.register(cmd::TAB_LOCK, |ctx| {
        let tab = ctx.active_controller();
        tab.set_locked(!tab.locked());
        // A lock change can show/hide the bar of this side.
        ctx.host(ctx.active_side()).tabs_changed.emit(&());
    });
}

/// Tab bars of both sides (state in `ctx.ext`; keeps the bars alive).
#[derive(Default)]
pub struct TabBars {
    bars: [RefCell<Option<Rc<TabBar>>>; 2],
}

impl TabBars {
    /// Bar of `side`, once its panel view exists.
    pub fn bar(&self, side: usize) -> Option<Rc<TabBar>> {
        self.bars[side.min(1)].borrow().clone()
    }
}

/// Create the (initially empty) state.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(TabBars::default()));
}

/// Build the tab bar of `view`.
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    let bar = TabBar::install(ctx, view);
    if let Some(state) = ctx.ext.get::<TabBars>() {
        *state.bars[view.side.min(1)].borrow_mut() = Some(bar);
    }
}

/// Activate the neighbour tab, wrapping around.
fn step(host: &Rc<PanelHost>, delta: isize) {
    let n = host.tab_count();
    if n < 2 {
        return;
    }
    let cur = host.active_index();
    let next = (cur as isize + delta).rem_euclid(n as isize) as usize;
    host.set_active(next);
}

/// Short tab caption for `loc`.
pub fn tab_title(loc: &Location) -> String {
    fn last(p: &Path) -> String {
        match p.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => "/".to_owned(),
        }
    }
    match loc {
        Location::Local(p) => last(p),
        Location::Archive { archive, inner } if inner.as_os_str().is_empty() => last(archive),
        Location::Archive { inner, .. } => last(inner),
        Location::List { .. } => tr("List").to_owned(),
    }
}

/// The tab bar of one panel side.
pub struct TabBar {
    root: gtk::Box,
    host: Rc<PanelHost>,
    /// Controllers whose `changed` signal is already connected (captions follow navigation).
    watched: RefCell<Vec<Weak<PanelController>>>,
}

impl TabBar {
    /// Build the bar for `view`'s side, put it into `view.tab_bar_slot` and keep it in sync.
    pub fn install(ctx: &Rc<AppContext>, view: &PanelView) -> Rc<TabBar> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 1);
        root.add_css_class("ofm-tabbar");
        root.set_visible(false);
        view.tab_bar_slot.append(&root);
        let bar = Rc::new(TabBar {
            root,
            host: Rc::clone(ctx.host(view.side)),
            watched: RefCell::new(Vec::new()),
        });
        bar.rebuild();
        let weak = Rc::downgrade(&bar);
        bar.host.tabs_changed.connect({
            let weak = weak.clone();
            move |()| {
                if let Some(b) = weak.upgrade() {
                    b.rebuild();
                }
            }
        });
        bar.host.active_changed.connect(move |_| {
            if let Some(b) = weak.upgrade() {
                b.rebuild();
            }
        });
        bar
    }

    /// Whether the bar is currently shown (tests).
    pub fn is_visible(&self) -> bool {
        self.root.is_visible()
    }

    /// Captions of the tabs in order (tests, accessibility).
    pub fn captions(&self) -> Vec<String> {
        self.host
            .tabs()
            .iter()
            .map(|t| tab_title(&t.location()))
            .collect()
    }

    fn watch(self: &Rc<Self>, tab: &Rc<PanelController>) {
        let mut watched = self.watched.borrow_mut();
        watched.retain(|w| w.upgrade().is_some());
        if watched
            .iter()
            .any(|w| w.upgrade().is_some_and(|t| Rc::ptr_eq(&t, tab)))
        {
            return;
        }
        watched.push(Rc::downgrade(tab));
        let weak = Rc::downgrade(self);
        tab.changed.connect(move |ev| {
            if matches!(ev, PanelEvent::Location | PanelEvent::State)
                && let Some(b) = weak.upgrade()
            {
                b.rebuild();
            }
        });
    }

    fn rebuild(self: &Rc<Self>) {
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        let tabs = self.host.tabs();
        let active = self.host.active_index();
        let any_locked = tabs.iter().any(|t| t.locked());
        self.root.set_visible(tabs.len() > 1 || any_locked);
        for (i, tab) in tabs.iter().enumerate() {
            self.watch(tab);
            let item = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            item.add_css_class("ofm-tab");
            if i == active {
                item.add_css_class("active");
            }
            if tab.locked() {
                item.append(&gtk::Image::from_icon_name("changes-prevent-symbolic"));
            }
            let title = tab_title(&tab.location());
            let label = gtk::Label::new(Some(&title));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(24);
            item.append(&label);
            item.set_tooltip_text(Some(&crate::search::lists::location_label(
                &tab.services().lists,
                &tab.location(),
            )));
            item.update_property(&[gtk::accessible::Property::Label(&title)]);
            // Mouse: click activates, middle click closes. Keys use the tab.* commands.
            let click = gtk::GestureClick::new();
            click.set_button(0);
            let host = Rc::clone(&self.host);
            let ctrl = Rc::downgrade(tab);
            click.connect_pressed(move |g, _, _, _| {
                let Some(ctrl) = ctrl.upgrade() else { return };
                let Some(idx) = host.tabs().iter().position(|t| Rc::ptr_eq(t, &ctrl)) else {
                    return;
                };
                let button = g.current_button();
                let host = Rc::clone(&host);
                // The bar is rebuilt by the change: do it after this event handler returned.
                gtk::glib::idle_add_local_once(move || match button {
                    1 => host.set_active(idx),
                    2 => {
                        host.close_tab(idx);
                    }
                    _ => {}
                });
            });
            item.add_controller(click);
            self.root.append(&item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(tab_title(&Location::Local("/a/b".into())), "b");
        assert_eq!(tab_title(&Location::Local("/".into())), "/");
        assert_eq!(
            tab_title(&Location::Archive {
                archive: "/d/x.zip".into(),
                inner: "".into()
            }),
            "x.zip"
        );
    }
}
