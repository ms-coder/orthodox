//! Content mode of the comparison: the byte-for-byte pass over same-name, same-size pairs.
//!
//! TODO(T8.1b): the `CompareContent` job (`crates/ofm-jobs/src/compare.rs`) is not on `main`
//! yet. When it lands, replace [`start`] with a submission of the candidate pairs (local paths of
//! both panels joined with the pair names), feed each streamed result through
//! `ofm_core::compare::apply_content_result`, re-run `marks::replace_selection` on both panels
//! as results arrive ("comparing content..." in the message row), and cancel the job on Esc
//! (`panel.cancel`). Until then the metadata marks are applied and the message says so.

use std::rc::Rc;

use ofm_core::compare::ContentCandidates;

use crate::context::AppContext;
use crate::i18n::tr;

/// Text appended to the summary while the content job is not available.
pub fn unavailable_note(pending: usize) -> String {
    format!(
        "{} ({} {})",
        tr("content compare not available yet"),
        pending,
        tr("files not compared")
    )
}

/// Start the content comparison of `candidates`. Returns `false` when it cannot run (always, for
/// now: see the module docs), so the caller reports [`unavailable_note`].
pub fn start(_ctx: &Rc<AppContext>, _candidates: ContentCandidates) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_the_count() {
        assert_eq!(
            unavailable_note(3),
            "content compare not available yet (3 files not compared)"
        );
    }
}
