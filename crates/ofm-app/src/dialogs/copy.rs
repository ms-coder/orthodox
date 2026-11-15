//! Copy/move dialog (T3.3 owns this file; F13, F15). Editable target (default: the other panel's
//! directory), rename mask, queue/parallel switch, options (follow symlinks, sync). Single item
//! copied into its own directory asks for a new name (`rename_mask` literal). Submits
//! `JobSpec::Copy`/`Move` through `jobs::controller(ctx)`. "Rename in place" (`file.rename`,
//! Shift+F6) is a `Move` into the same directory with a literal mask: use the same dialog.

use std::rc::Rc;

use crate::context::AppContext;

/// Which transfer the dialog performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// F5.
    Copy,
    /// F6.
    Move,
    /// Shift+F6 (name field only).
    Rename,
}

/// Show the dialog for the active panel's selection (or the item under the cursor).
pub fn present(_ctx: &Rc<AppContext>, _kind: TransferKind) {
    todo!("T3.3")
}
