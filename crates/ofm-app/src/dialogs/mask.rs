//! Select/unselect-by-mask dialog (Gray+ / Gray-), with mask history. Owner: T2.3 (F09).

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;

/// Register `select.by_mask` / `select.unmask` (the dialog itself; `select.invert` etc. are in
/// `navigation/selection.rs`).
pub fn install_commands(_reg: &mut CommandRegistry) {}

/// Create the mask history in `ctx.ext`.
pub fn install_state(_ctx: &Rc<AppContext>) {}
