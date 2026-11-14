//! Panel state on the UI side (frozen core, T2.0). A *panel side* (left/right) is a
//! [`PanelHost`] holding tabs; each tab is a [`PanelController`] owning a `PanelModel`, its
//! location, the load generation and navigation history. Views (`FileList`, status line, tab bar)
//! observe controllers through [`PanelController::changed`]; commands act on controllers.

mod controller;
mod history;
mod host;

pub use controller::{LoadOpts, LoadState, PanelController, PanelEvent, ViewGeometry};
pub use history::NavHistory;
pub use host::PanelHost;

/// Left or right panel: index into `AppContext::panels`.
pub type Side = usize;
/// Left panel.
pub const LEFT: Side = 0;
/// Right panel.
pub const RIGHT: Side = 1;
