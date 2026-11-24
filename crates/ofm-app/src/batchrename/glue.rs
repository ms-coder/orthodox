//! Pure glue of the batch rename flow (no GTK, no file system): selection -> editor text, error
//! texts, preview lines, undo-log locations. Unit-tested here; the flow in `flow.rs` only wires
//! these to dialogs, the editor and the job engine.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use ofm_core::batch_rename::{BatchError, InvalidNameReason, Plan, StepKind, to_editor_text};

use crate::i18n::tr;

/// Sub-directory of the XDG state directory that holds the undo logs.
pub const LOG_DIR: &str = "batchrename";

/// File name (inside [`LOG_DIR`]) of the log of the last finished batch: what the undo command
/// reverses. Only the last batch is kept.
pub const LAST_LOG: &str = "last.toml";

/// What goes into the editor buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The names that are in the buffer, in line order (what `parse_edited` must be given).
    pub editor_names: Vec<OsString>,
    /// The buffer text.
    pub text: String,
    /// Names that cannot round trip through an editor; left out and not renamed.
    pub unsupported: Vec<OsString>,
}

/// Selection -> editor buffer. `Err` carries the refused names when none can be edited as text
/// (empty when the selection itself was empty); the caller tells the user which were refused.
pub fn prepare(names: &[OsString]) -> Result<Prepared, Vec<OsString>> {
    let editor = to_editor_text(names);
    let editor_names: Vec<OsString> = names
        .iter()
        .filter(|n| !editor.unsupported.contains(n))
        .cloned()
        .collect();
    if editor_names.is_empty() {
        return Err(editor.unsupported);
    }
    Ok(Prepared {
        editor_names,
        text: editor.text,
        unsupported: editor.unsupported,
    })
}

/// All names that exist in the directory as far as the UI knows: the fresh listing plus the
/// names the panel showed (a name that vanished meanwhile only makes the check stricter).
pub fn existing_names(fresh: Vec<OsString>, panel: &[OsString]) -> HashSet<OsString> {
    let mut all: HashSet<OsString> = fresh.into_iter().collect();
    all.extend(panel.iter().cloned());
    all
}

/// `a, b, c` for display (lossy), cut after `max` names with `, ... (+N)`.
pub fn name_list(names: &[OsString], max: usize) -> String {
    let mut out: Vec<String> = names
        .iter()
        .take(max)
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    if names.len() > max {
        out.push(format!("... (+{})", names.len() - max));
    }
    out.join(", ")
}

fn reason_text(reason: InvalidNameReason) -> &'static str {
    match reason {
        InvalidNameReason::Empty => tr("a name cannot be empty"),
        InvalidNameReason::Dot => tr("'.' is not a file name"),
        InvalidNameReason::DotDot => tr("'..' is not a file name"),
        InvalidNameReason::Slash => tr("a name cannot contain '/'"),
        InvalidNameReason::Nul => tr("a name cannot contain a NUL character"),
    }
}

/// Why the edited text cannot be applied, in words for the problem dialog.
pub fn problem_text(e: &BatchError) -> String {
    let q = |s: &OsStr| format!("'{}'", s.to_string_lossy());
    match e {
        BatchError::LineCount {
            expected,
            actual,
            added,
            removed,
        } => {
            let what = if *added > 0 {
                format!("{added} {}", tr("line(s) added"))
            } else {
                format!("{removed} {}", tr("line(s) removed"))
            };
            format!(
                "{} {actual}, {} {expected} ({what}). {}",
                tr("The text has"),
                tr("expected"),
                tr("Keep exactly one line per file; do not add, delete or reorder lines.")
            )
        }
        BatchError::InvalidTarget { name, reason } => {
            format!(
                "{} {}: {}.",
                tr("Invalid new name"),
                q(name),
                reason_text(*reason)
            )
        }
        BatchError::DuplicateTarget(n) => {
            format!("{} {}.", tr("Two files would get the same new name:"), q(n))
        }
        BatchError::DuplicateSource(n) => {
            format!("{} {}.", tr("The same file appears twice:"), q(n))
        }
        BatchError::Collision { target } => format!(
            "{} {} {}",
            tr("The name"),
            q(target),
            tr("already exists in this folder and is not being renamed.")
        ),
        BatchError::UnsupportedNames(names) => format!(
            "{} {}",
            tr("These names cannot be edited as text:"),
            name_list(names, 5)
        ),
        BatchError::StaleUndo(why) => format!(
            "{} {why}",
            tr("The last batch rename can no longer be undone safely:")
        ),
        BatchError::InternalCycle => tr("Internal error: unresolved rename cycle.").to_owned(),
        BatchError::Io { path, source } => format!("{}: {source}", path.to_string_lossy()),
        BatchError::Serialize(msg) => msg.clone(),
    }
}

/// `old -> new` lines of the preview, in the order the user listed them.
pub fn preview_lines(preview: &[(OsString, OsString)]) -> Vec<String> {
    preview
        .iter()
        .map(|(from, to)| {
            format!(
                "{}  \u{2192}  {}",
                from.to_string_lossy(),
                to.to_string_lossy()
            )
        })
        .collect()
}

/// Number of temporary-name steps the plan needs (swaps, cycles); they are not listed in the
/// preview, the dialog only mentions them.
pub fn temp_steps(plan: &Plan) -> usize {
    plan.steps
        .iter()
        .filter(|s| s.kind != StepKind::Direct)
        .count()
}

/// The log of the batch whose job is `unique` (never reuses a name, so no log of an earlier
/// batch is overwritten while its job runs).
pub fn pending_log(state_dir: &Path, unique: &str) -> PathBuf {
    state_dir
        .join(LOG_DIR)
        .join(format!("pending-{unique}.toml"))
}

/// The log the undo command reads.
pub fn last_log(state_dir: &Path) -> PathBuf {
    state_dir.join(LOG_DIR).join(LAST_LOG)
}

/// Unique-enough id for one batch (process id, nanoseconds, counter): names logs and the
/// temporary directory.
pub fn unique_id(pid: u32, nanos: u128, seq: u64) -> String {
    format!("{pid}-{nanos:x}-{seq}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use ofm_core::batch_rename::{RenameOp, plan};

    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn selection_becomes_editor_text_and_excludes_unsupported_names() {
        let bad = OsString::from_vec(b"b\xff".to_vec());
        let nl = os("with\nnewline");
        let names = vec![os("a.txt"), bad.clone(), os("c d"), nl.clone()];
        let p = prepare(&names).unwrap();
        assert_eq!(p.text, "a.txt\nc d\n");
        assert_eq!(p.editor_names, [os("a.txt"), os("c d")]);
        assert_eq!(p.unsupported, [bad, nl]);
    }

    #[test]
    fn all_unsupported_is_an_error_naming_them() {
        let bad = OsString::from_vec(b"\xfe".to_vec());
        assert_eq!(prepare(std::slice::from_ref(&bad)), Err(vec![bad]));
        assert_eq!(prepare(&[]), Err(vec![]));
    }

    #[test]
    fn existing_names_merges_both_sources() {
        let all = existing_names(vec![os("a"), os("b")], &[os("b"), os("c")]);
        assert_eq!(all.len(), 3);
        assert!(all.contains(&os("c")));
    }

    #[test]
    fn name_list_is_cut() {
        let names: Vec<OsString> = (0..8).map(|i| os(&format!("n{i}"))).collect();
        assert_eq!(name_list(&names, 3), "n0, n1, n2, ... (+5)");
        assert_eq!(name_list(&names[..2], 3), "n0, n1");
    }

    #[test]
    fn engine_errors_map_to_readable_text() {
        let t = problem_text(&BatchError::LineCount {
            expected: 3,
            actual: 5,
            added: 2,
            removed: 0,
        });
        assert!(
            t.contains("5") && t.contains("3") && t.contains("2 line(s) added"),
            "{t}"
        );
        let t = problem_text(&BatchError::LineCount {
            expected: 3,
            actual: 1,
            added: 0,
            removed: 2,
        });
        assert!(t.contains("2 line(s) removed"), "{t}");
        let t = problem_text(&BatchError::InvalidTarget {
            name: os("a/b"),
            reason: InvalidNameReason::Slash,
        });
        assert!(t.contains("'a/b'") && t.contains("'/'"), "{t}");
        assert!(problem_text(&BatchError::DuplicateTarget(os("x"))).contains("'x'"));
        assert!(problem_text(&BatchError::DuplicateSource(os("x"))).contains("'x'"));
        let t = problem_text(&BatchError::Collision {
            target: os("b.txt"),
        });
        assert!(t.contains("'b.txt'") && t.contains("already exists"), "{t}");
        assert!(problem_text(&BatchError::StaleUndo("step 0".into())).contains("step 0"));
    }

    #[test]
    fn preview_lists_user_visible_renames_only_and_counts_temp_steps() {
        let existing: HashSet<OsString> = [os("a"), os("b"), os("c")].into_iter().collect();
        let swap = [
            RenameOp {
                from: os("a"),
                to: os("b"),
            },
            RenameOp {
                from: os("b"),
                to: os("a"),
            },
        ];
        let p = plan(&swap, &existing).unwrap();
        assert_eq!(preview_lines(&p.preview).len(), 2);
        assert!(preview_lines(&p.preview)[0].starts_with("a  \u{2192}  b"));
        assert!(temp_steps(&p) >= 2);
        let simple = plan(
            &[RenameOp {
                from: os("c"),
                to: os("d"),
            }],
            &existing,
        )
        .unwrap();
        assert_eq!(temp_steps(&simple), 0);
    }

    #[test]
    fn log_paths_live_under_the_state_dir_and_never_collide() {
        let state = Path::new("/s/ofm");
        assert_eq!(last_log(state), Path::new("/s/ofm/batchrename/last.toml"));
        let a = pending_log(state, &unique_id(1, 2, 3));
        let b = pending_log(state, &unique_id(1, 2, 4));
        assert_ne!(a, b);
        assert!(a.starts_with("/s/ofm/batchrename"));
        assert_ne!(a, last_log(state));
    }
}
