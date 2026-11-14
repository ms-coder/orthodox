//! Window chrome: F-key bar, F9 menu bar, status line, active-panel decoration. Owner: T2.2
//! (everything under `chrome/`). Empty skeleton from T2.0: add files per widget, e.g.
//! `fkeybar.rs`, `menubar.rs`, `statusline.rs`, and call them from the hooks below.
//!
//! Widgets go into the slots of `MainWindow` (`menubar_slot`, `status_slot`, `fkeybar_slot`);
//! they are built from the command registry (`ofm_core::commands::ALL`) and the keymap, and run
//! commands via `ctx.execute(cmd)` only.

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::window::MainWindow;

/// Register chrome commands (`app.menu`, `app.theme_next`, `app.reload_config`, ...).
pub fn install_commands(_reg: &mut CommandRegistry) {}

/// Create chrome state in `ctx.ext`.
pub fn install_state(_ctx: &Rc<AppContext>) {}

/// Build F-key bar / menu bar / status line into the window slots.
pub fn install_ui(_ctx: &Rc<AppContext>, _win: &MainWindow) {}
