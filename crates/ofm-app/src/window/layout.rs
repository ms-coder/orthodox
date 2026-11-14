//! Panel layout: the split ratio of the `gtk::Paned` (kept proportional while the window is
//! resized, persisted in the session) and one-panel mode (the inactive panel is hidden).

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use ofm_config::session::Session;

use super::main_window::MainWindow;
use crate::context::AppContext;
use crate::panel::LEFT;

/// Smallest and largest split ratio (a panel never collapses completely).
const RATIO_RANGE: (f32, f32) = (0.05, 0.95);
/// Width assumed for converting the stored ratio before the window has a size.
const DEFAULT_WIDTH: i32 = 1200;

/// Layout state in `ctx.ext`.
pub struct LayoutState {
    ratio: Cell<f32>,
    one_panel: Cell<bool>,
}

impl LayoutState {
    /// State from a (sanitized) session.
    pub fn from_session(s: &Session) -> Self {
        LayoutState {
            ratio: Cell::new(clamp_ratio(s.split_ratio)),
            one_panel: Cell::new(s.one_panel),
        }
    }

    /// Left panel's share of the width.
    pub fn ratio(&self) -> f32 {
        self.ratio.get()
    }

    /// Set the ratio (clamped) without touching the widgets.
    pub fn set_ratio(&self, ratio: f32) {
        self.ratio.set(clamp_ratio(ratio));
    }

    /// One-panel mode on?
    pub fn one_panel(&self) -> bool {
        self.one_panel.get()
    }
}

/// Clamp a ratio into the allowed range; NaN gives 0.5.
pub fn clamp_ratio(r: f32) -> f32 {
    if r.is_nan() {
        0.5
    } else {
        r.clamp(RATIO_RANGE.0, RATIO_RANGE.1)
    }
}

/// Divider position in pixels for `ratio` of `width`.
pub fn ratio_to_position(ratio: f32, width: i32) -> i32 {
    (clamp_ratio(ratio) * width.max(0) as f32).round() as i32
}

/// Ratio for a divider at `position` of `width` (`None` while the width is unknown).
pub fn position_to_ratio(position: i32, width: i32) -> Option<f32> {
    (width > 0).then(|| clamp_ratio(position as f32 / width as f32))
}

/// Move the divider to the stored ratio.
fn apply_ratio(state: &LayoutState, win: &MainWindow, width: i32) {
    let pos = ratio_to_position(state.ratio(), width);
    if win.panels.position() != pos {
        win.panels.set_position(pos);
    }
}

/// Show/hide the panels according to one-panel mode: the inactive side is hidden.
pub fn apply_one_panel(ctx: &AppContext, win: &MainWindow) {
    let Some(state) = ctx.ext.get::<LayoutState>() else {
        return;
    };
    let one = state.one_panel();
    let active = ctx.active_side();
    win.left.root.set_visible(!one || active == LEFT);
    win.right.root.set_visible(!one || active != LEFT);
}

/// `panel.toggle_other`: hide/show the inactive panel.
pub fn toggle_one_panel(ctx: &Rc<AppContext>) {
    let Some(state) = ctx.ext.get::<LayoutState>() else {
        return;
    };
    state.one_panel.set(!state.one_panel.get());
    if let Some(win) = ctx.window() {
        apply_one_panel(ctx, win);
        if !state.one_panel() {
            apply_ratio(&state, win, win.panels.width());
        }
    }
}

/// Wire the paned: initial position, proportional resize, drag tracking, one-panel mode.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    let Some(state) = ctx.ext.get::<LayoutState>() else {
        return;
    };
    // Initial position from the stored ratio and the size we are going to open with.
    let width = ctx
        .ext
        .get::<super::session::SessionState>()
        .map(|s| s.loaded.window.width)
        .filter(|w| *w > 0)
        .unwrap_or(DEFAULT_WIDTH);
    win.panels
        .set_position(ratio_to_position(state.ratio(), width));

    // A drag that moved the divider updates the ratio. Reading `notify::position` instead would
    // also catch GTK's own re-allocations (hiding a panel, resizing) and corrupt the ratio.
    {
        let st = Rc::clone(&state);
        let panels = win.panels.clone();
        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        drag.connect_drag_end(move |_, dx, _| {
            if dx.abs() < 1.0 || st.one_panel() {
                return;
            }
            if let Some(r) = position_to_ratio(panels.position(), panels.width()) {
                st.set_ratio(r);
            }
        });
        win.panels.add_controller(drag);
    }

    // Keep the proportion when the window size changes: the surface reports its new width before
    // the paned has been re-allocated.
    {
        let st = Rc::clone(&state);
        let panels = win.panels.clone();
        win.window.connect_realize(move |w| {
            let Some(surface) = w.surface() else { return };
            let st = Rc::clone(&st);
            let panels = panels.clone();
            surface.connect_layout(move |_, width, _| {
                if st.one_panel() {
                    return;
                }
                let pos = ratio_to_position(st.ratio(), width);
                if panels.position() != pos {
                    panels.set_position(pos);
                }
            });
        });
    }

    // One-panel mode follows the active panel.
    apply_one_panel(ctx, win);
    {
        let weak = Rc::downgrade(ctx);
        ctx.active_changed.connect(move |_| {
            if let Some(ctx) = weak.upgrade()
                && let Some(win) = ctx.window()
            {
                apply_one_panel(&ctx, win);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_conversions() {
        assert_eq!(ratio_to_position(0.5, 1000), 500);
        assert_eq!(ratio_to_position(0.3, 1000), 300);
        assert_eq!(ratio_to_position(2.0, 1000), 950);
        assert_eq!(ratio_to_position(-1.0, 1000), 50);
        assert_eq!(ratio_to_position(0.5, -10), 0);
        assert_eq!(position_to_ratio(250, 1000), Some(0.25));
        assert_eq!(position_to_ratio(0, 1000), Some(0.05));
        assert_eq!(position_to_ratio(10, 0), None);
        assert_eq!(clamp_ratio(f32::NAN), 0.5);
    }

    #[test]
    fn state_from_session_is_clamped() {
        let s = Session {
            split_ratio: 7.0,
            one_panel: true,
            ..Session::default()
        };
        let st = LayoutState::from_session(&s);
        assert_eq!(st.ratio(), 0.95);
        assert!(st.one_panel());
    }
}
