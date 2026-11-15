//! Make-directory dialog (T3.3 owns this file; F13, F7): name entry (may contain `/` for nested
//! directories), submits `JobSpec::Mkdir`; after the job the panel cursor goes to the new
//! directory (`Report::created`).

use std::rc::Rc;

use crate::context::AppContext;

/// Ask for a name and create the directory in the active panel.
pub fn present(_ctx: &Rc<AppContext>) {
    todo!("T3.3")
}
