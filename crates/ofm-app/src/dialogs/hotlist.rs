//! Directory hotlist popup (Ctrl+D) and editor, `panel.hotlist`, `panel.hotlist_add`. Owner: T2.3 (F07).

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;

/// Register the hotlist commands.
pub fn install_commands(_reg: &mut CommandRegistry) {}

/// Load/create the hotlist state in `ctx.ext` (persisted through `ofm-config`'s history/session files).
pub fn install_state(_ctx: &Rc<AppContext>) {}
