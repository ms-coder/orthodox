//! Main window and panel views. Owner: T2.2 (everything under `window/`).
//!
//! T2.0 provides the layout with named regions ("slots") that other modules fill through the
//! hooks in `hooks.rs`, so nobody edits `main_window.rs` just to add a widget:
//!
//! ```text
//! +----------------------------------------------+
//! | menubar_slot            (chrome: F9 menu)    |
//! +----------------------+-----------------------+
//! | PanelView (left)     | PanelView (right)     |   gtk::Paned (`panels`)
//! |  tab_bar_slot        |  tab_bar_slot         |
//! |  path header         |  path header          |
//! |  overlay[FileList]   |  overlay[FileList]    |
//! |  status_slot         |  status_slot          |
//! +----------------------+-----------------------+
//! | terminal_slot (M4)                           |
//! | cmdline_slot  (M4)                           |
//! | fkeybar_slot            (chrome: F-key bar)  |
//! +----------------------------------------------+
//! ```

mod commands;
pub mod layout;
mod main_window;
mod panel_view;
pub mod session;

pub use commands::{install_commands, install_state};
pub use main_window::{MainWindow, install_ui};
pub use panel_view::{PanelView, TextHold, install_panel_view};
