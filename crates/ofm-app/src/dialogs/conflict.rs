//! Question dialog (T3.3 owns this file; F16): one dialog for every `ofm_jobs::Question`:
//! `Conflict` (both sides' size/date; choices overwrite / skip / rename new / rename existing /
//! overwrite if newer / overwrite if size differs / compare, "apply to all"; honour
//! `ConflictKind::allows` by disabling forbidden choices), `Error` (retry / skip / abort, path +
//! message + errno hint, "apply to all" for skip) and `TrashUnavailable` (delete permanently /
//! skip). Must answer through `reply.answer(..)` exactly once; closing the dialog = drop the
//! responder (aborts the job). Holds `ctx.enter_modal()` while open.

use std::rc::Rc;

use ofm_core::Responder;
use ofm_jobs::{Answer, JobId, Question};

use crate::context::AppContext;

/// Show the dialog for `question` of job `id`.
pub fn present(_ctx: &Rc<AppContext>, _id: JobId, _question: Question, _reply: Responder<Answer>) {
    todo!("T3.3")
}
