//! Create-link dialog (T3.3 owns this file; F18, Alt+F6): symbolic (absolute or relative target via
//! `ofm_jobs::ops::link::relative_target`) or hard link; submits `JobSpec::Link`.

use std::rc::Rc;

use crate::context::AppContext;

/// Ask for link path/kind for the item under the cursor and create it.
pub fn present(_ctx: &Rc<AppContext>) {
    todo!("T3.3")
}
