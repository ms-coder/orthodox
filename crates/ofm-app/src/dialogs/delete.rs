//! Delete confirmation (T3.3 owns this file; F14): names/counts, "Move to Trash" vs "Delete
//! permanently" (`permanent == true` for Shift+Del). Submits `JobSpec::Delete`.

use std::rc::Rc;

use crate::context::AppContext;

/// Confirm and submit deletion of the active panel's selection.
pub fn present(_ctx: &Rc<AppContext>, _permanent: bool) {
    todo!("T3.3")
}
