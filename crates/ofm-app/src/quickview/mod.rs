//! Quick view (F22, Ctrl+Q; T5.2b): the OTHER panel turns into a preview of the item under the
//! cursor of the active panel; Ctrl+Q again restores it.
//!
//! - **Pane**: [`pane::PreviewPane`] is an overlay child over the other panel's file list, so the
//!   list (cursor, selection, scroll) is untouched while previewing.
//! - **Following**: the controller listens to the source panel (the one that is not showing the
//!   pane) and re-targets after a [`DEBOUNCE`] of quiet; stale work is cancelled
//!   ([`CancelToken`](ofm_core::CancelToken)) and its results dropped by generation.
//! - **Work**: [`worker`] — text through `ofm-viewer`, images through gdk-pixbuf (decoded on a
//!   worker, uploaded as a `gdk::MemoryTexture` here), directory summaries through the jobs size
//!   walk, everything else as an information table. The GTK thread never touches the file system.
//! - Keys: only the command `panel.toggle_quickview` (registered here); while the preview is
//!   on, switching the active panel to the preview side bounces back to the file panel.

mod controller;
pub mod debounce;
pub mod pane;
pub mod worker;

use std::rc::Rc;
use std::time::Duration;

use ofm_core::cmd;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::window::PanelView;

pub use controller::QuickView;

/// Quiet time after the last cursor move before a new preview starts (F22: at most 100 ms).
pub const DEBOUNCE: Duration = Duration::from_millis(60);

/// Register `panel.toggle_quickview`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_TOGGLE_QUICKVIEW, |ctx| {
        if let Some(qv) = ctx.ext.get::<QuickView>() {
            qv.toggle(ctx);
        }
    });
}

/// Create the controller in `ctx.ext`.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(QuickView::new(ctx));
}

/// Add the (hidden) preview pane over this side's file list and keep it themed.
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    let Some(qv) = ctx.ext.get::<QuickView>() else {
        tracing::error!("quickview: install_state did not run before install_panel_view");
        return;
    };
    let pane = pane::PreviewPane::new();
    pane.set_palette(ctx.theme.palette());
    view.overlay.add_overlay(&pane);
    view.overlay.set_measure_overlay(&pane, false);
    view.overlay.set_clip_overlay(&pane, true);
    {
        let pane = pane.clone();
        ctx.theme
            .changed
            .connect(move |palette| pane.set_palette(Rc::clone(palette)));
    }
    qv.register_pane(view.side, pane);
    qv.register_hold(view.side, view.text_hold(ctx));
}
