//! The extension hooks (FROZEN: edited only by the architect/lead). Each parallel task owns the
//! body of exactly the functions named after its modules; this file just calls them in a fixed
//! order, so no two tasks ever edit the same file.
//!
//! Startup order (see `app::build_context` and `window::MainWindow::new`):
//! 1. [`install_commands`]  - fill the `CommandRegistry` (no `AppContext` yet).
//! 2. [`install_state`]     - create module state in `ctx.ext` (context exists, window does not).
//! 3. window is built; for each panel side [`install_panel_view`] runs on its `PanelView`;
//!    then [`install_ui`] runs once with the finished `MainWindow` (fill chrome slots etc.).

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::window::{MainWindow, PanelView};
use crate::{chrome, dialogs, jobs, navigation, widgets, window};

/// Register command handlers. Every module registers only commands it owns (OWNERSHIP.md);
/// duplicates are detected by a test.
pub fn install_commands(reg: &mut CommandRegistry) {
    widgets::file_list::install_commands(reg); // T2.1: cursor, view mode, sort, hidden
    window::install_commands(reg); // T2.2: app.*, window.*, panel.switch/toggle_other/swap
    chrome::install_commands(reg); // T2.2: app.menu, app.theme_next, ...
    navigation::install_commands(reg); // T2.3: go_*, history, tabs, select.*, quicksearch, ...
    dialogs::install_commands(reg); // T2.3: mask + hotlist dialogs' commands
    jobs::install_commands(reg); // T3.3: file.copy/move/delete/..., app.jobs, jobs.*
}

/// Create module-owned state and store it with `ctx.ext.insert(Rc::new(..))`.
pub fn install_state(ctx: &Rc<AppContext>) {
    window::install_state(ctx); // T2.2
    chrome::install_state(ctx); // T2.2
    navigation::install_state(ctx); // T2.3
    dialogs::install_state(ctx); // T2.3
    jobs::install_state(ctx); // T3.3: JobsController in ctx.ext
}

/// Decorate one panel view right after it is built (tab bar, quick-search popover, ...).
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    window::install_panel_view(ctx, view); // T2.2
    navigation::install_panel_view(ctx, view); // T2.3
}

/// Fill window regions once the whole `MainWindow` exists (menu bar, F-key bar, status line).
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    window::install_ui(ctx, win); // T2.2
    chrome::install_ui(ctx, win); // T2.2
    navigation::install_ui(ctx, win); // T2.3
    jobs::install_ui(ctx, win); // T3.3: jobs view, progress in status line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two modules must never register the same command (headless: no GTK needed).
    #[test]
    fn no_command_is_registered_twice() {
        let mut reg = CommandRegistry::new();
        install_commands(&mut reg);
        assert!(reg.duplicates().is_empty(), "{:?}", reg.duplicates());
        assert!(reg.has(ofm_core::cmd::PANEL_SWITCH));
        assert!(reg.has(ofm_core::cmd::PANEL_CURSOR_DOWN));
    }
}
