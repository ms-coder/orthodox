//! Properties dialog (T3.3 owns this file; F19, Alt+Enter): chmod (octal entry + checkboxes),
//! owner/group (only if permitted), recursive option, directory size (`JobSpec::CalcSize`, result
//! `Outcome::Size`, cancellable). Applies through `JobSpec::SetAttrs`.

use std::rc::Rc;

use crate::context::AppContext;

/// Show properties of the selection / item under the cursor.
pub fn present(_ctx: &Rc<AppContext>) {
    todo!("T3.3")
}
