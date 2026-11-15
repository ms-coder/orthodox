//! Events from workers to the UI, including questions the UI answers (frozen by T3.0; protocol in
//! ADR-0006).
//!
//! Flow: `JobEngine::events()` yields [`JobEvent`]s in per-job order
//! `Queued -> Started -> (Progress | Ask | StateChanged)* -> Finished`. The UI must consume them
//! with ONE receiver (the receiver is a work queue, not a broadcast).

use std::ffi::OsString;
use std::path::PathBuf;

use ofm_core::Responder;

use crate::fs::{FileType, FsStat};
use crate::spec::{JobId, RunMode};

/// Lifecycle state of a job, for the jobs view and the quit prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Waiting in the queue lane.
    Queued,
    /// Running.
    Running,
    /// Paused at the next safe point (between files / 1 MiB chunks).
    Paused,
    /// Done (any outcome).
    Finished,
}

/// Throttled (<= 10 Hz per job, never blocks the worker) progress snapshot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    /// True while the plan phase is still counting: totals are growing, no ETA yet.
    pub planning: bool,
    /// Files done / total (total known after the plan phase).
    pub files_done: u64,
    /// Total files, 0 while unknown.
    pub files_total: u64,
    /// Bytes done.
    pub bytes_done: u64,
    /// Total bytes, 0 while unknown.
    pub bytes_total: u64,
    /// Current speed in bytes/s (smoothed).
    pub bytes_per_sec: u64,
    /// Estimated remaining seconds, if computable.
    pub eta_secs: Option<u64>,
    /// File currently being processed.
    pub current: Option<PathBuf>,
}

/// A non-fatal problem worth showing in the job report (xattr not copied, special file skipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Path concerned.
    pub path: PathBuf,
    /// Human-readable message.
    pub message: String,
}

/// What a finished (or cancelled) job did. Carried by every [`Outcome`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Non-fatal warnings.
    pub warnings: Vec<Warning>,
    /// Items skipped by the user's choice (conflict "skip", error "skip", ...).
    pub skipped: u64,
    /// Files fully processed.
    pub files_done: u64,
    /// Bytes processed.
    pub bytes_done: u64,
    /// Final paths of the top-level items this job created or moved into place (copy/move
    /// destinations after rename masks and conflict renames; mkdir/link results). The UI puts the
    /// panel cursor on the first of these after the refresh.
    pub created: Vec<PathBuf>,
}

/// Final result of a job.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Everything succeeded (skips and warnings are in the report).
    Ok(Report),
    /// Cancelled or aborted by the user; no half-written destination files remain; whatever was
    /// completed before stays (and is in the report).
    Cancelled(Report),
    /// Failed; the message already names the path. Details of per-item problems were offered
    /// through `Ask` (retry/skip/abort) or are in `report.warnings`.
    Failed {
        /// Short message for the UI (`/path: Permission denied (os error 13)`).
        error: String,
        /// Path the error is about, when there is one.
        path: Option<PathBuf>,
        /// Progress made before the failure.
        report: Report,
    },
    /// Result of `CalcSize`.
    Size(SizeTotals),
}

impl Outcome {
    /// True for [`Outcome::Ok`] and [`Outcome::Size`].
    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Ok(_) | Outcome::Size(_))
    }

    /// The report, if the outcome carries one.
    pub fn report(&self) -> Option<&Report> {
        match self {
            Outcome::Ok(r) | Outcome::Cancelled(r) | Outcome::Failed { report: r, .. } => Some(r),
            Outcome::Size(_) => None,
        }
    }
}

/// Result of `CalcSize` (hardlinks counted once, by dev+ino).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SizeTotals {
    /// Sum of apparent file sizes in bytes (`st_size`).
    pub bytes: u64,
    /// Sum of allocated bytes (`st_blocks * 512`).
    pub disk_bytes: u64,
    /// Number of non-directory entries (files, symlinks, specials).
    pub files: u64,
    /// Number of directories, including the roots passed in.
    pub dirs: u64,
}

/// Kind of an item in a conflict question.
pub type ItemKind = FileType;

/// Description of one side of a conflict (what the dialog shows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemInfo {
    /// Full path.
    pub path: PathBuf,
    /// File, dir, symlink, other.
    pub kind: ItemKind,
    /// Size in bytes.
    pub size: u64,
    /// Modification time, ns since epoch.
    pub mtime_ns: i128,
}

impl ItemInfo {
    /// Build from an `lstat` result.
    pub fn from_stat(path: impl Into<PathBuf>, st: &FsStat) -> Self {
        ItemInfo {
            path: path.into(),
            kind: st.file_type(),
            size: st.size,
            mtime_ns: st.mtime_ns,
        }
    }
}

/// Why a conflict is special; decides which answers are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Same kind on both sides (file/file, symlink/symlink): every answer is allowed.
    Replaceable,
    /// File vs directory: `Overwrite*` is refused (a directory tree is never replaced by a file
    /// or vice versa); rename/skip/abort remain.
    TypeMismatch,
    /// Source and destination are the same file (same dev+ino, e.g. copy into the same
    /// directory): `Overwrite*` is refused; `RenameNew` is the useful answer (F13).
    SameFile,
}

impl ConflictKind {
    /// Whether `answer` is acceptable for this kind of conflict.
    pub fn allows(self, answer: ConflictAnswer) -> bool {
        match answer {
            ConflictAnswer::Overwrite
            | ConflictAnswer::OverwriteIfNewer
            | ConflictAnswer::OverwriteIfSizeDiffers => self == ConflictKind::Replaceable,
            ConflictAnswer::Skip
            | ConflictAnswer::RenameNew
            | ConflictAnswer::RenameExisting
            | ConflictAnswer::Abort => true,
        }
    }
}

/// How to resolve a destination conflict (F16). `Compare` is a UI-only action (it shows both
/// sides and re-asks), so it is not an answer. "Append/resume" is not offered in v1.0 (ADR-0006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAnswer {
    /// Replace the destination (atomic rename over it).
    Overwrite,
    /// Leave the destination, skip this item.
    Skip,
    /// Overwrite only if the source is strictly newer (mtime), else skip.
    OverwriteIfNewer,
    /// Overwrite only if the sizes differ, else skip.
    OverwriteIfSizeDiffers,
    /// Keep both: write the new item under a fresh name (`name (1).ext`).
    RenameNew,
    /// Keep both: rename the existing destination out of the way (`name (1).ext`), then write the
    /// new item under the original name.
    RenameExisting,
    /// Abort the whole job (same effect as cancel).
    Abort,
}

/// How to react to an error on one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAction {
    /// Try again.
    Retry,
    /// Skip this item and continue.
    Skip,
    /// Abort the job (same effect as cancel).
    Abort,
}

/// What was being done when an error occurred (for the error dialog; English via `Display`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Scanning sources (plan phase).
    Scan,
    /// Copying file data or metadata.
    Copy,
    /// Moving (rename or copy+delete).
    Move,
    /// Deleting permanently.
    Delete,
    /// Moving to the trash.
    Trash,
    /// Creating a directory.
    Mkdir,
    /// Creating a link.
    Link,
    /// Changing mode/owner.
    SetAttrs,
    /// Renaming.
    Rename,
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Operation::Scan => "scan",
            Operation::Copy => "copy",
            Operation::Move => "move",
            Operation::Delete => "delete",
            Operation::Trash => "move to trash",
            Operation::Mkdir => "create directory",
            Operation::Link => "create link",
            Operation::SetAttrs => "change attributes",
            Operation::Rename => "rename",
        })
    }
}

/// A question a worker asks while running. The worker is blocked until it is answered (or the job
/// is cancelled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Destination exists. Answer with [`Answer::Conflict`]; `kind.allows(action)` must hold.
    Conflict {
        /// The incoming item.
        source: ItemInfo,
        /// What is already there.
        existing: ItemInfo,
        /// Which answers are allowed.
        kind: ConflictKind,
    },
    /// An operation failed on `path`. Answer with [`Answer::Error`].
    Error {
        /// Path.
        path: PathBuf,
        /// What was being done.
        operation: Operation,
        /// OS/library message.
        message: String,
        /// Raw OS error number when there is one (ENOSPC = 28 deserves a distinct hint).
        os_error: Option<i32>,
    },
    /// Trash is unavailable for this path: delete permanently? Answer with [`Answer::Confirm`].
    TrashUnavailable {
        /// Path that cannot be trashed.
        path: PathBuf,
    },
}

/// The UI's answer, matching the [`Question`] variant. A mismatching variant counts as "abort".
/// `apply_to_all` remembers the decision for the rest of the job (and is ignored where it cannot
/// apply, e.g. a remembered `Overwrite` is not used for a type mismatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// To `Question::Conflict`.
    Conflict {
        /// Decision.
        action: ConflictAnswer,
        /// Remember for the rest of the job.
        apply_to_all: bool,
    },
    /// To `Question::Error`.
    Error {
        /// Decision.
        action: ErrorAction,
        /// Remember for the rest of the job.
        apply_to_all: bool,
    },
    /// To `Question::TrashUnavailable`.
    Confirm {
        /// True = delete permanently, false = skip this item.
        permanent: bool,
        /// Remember for the rest of the job.
        apply_to_all: bool,
    },
}

/// Event stream item.
#[derive(Debug)]
pub enum JobEvent {
    /// Job accepted (emitted before `submit` returns).
    Queued {
        /// Job.
        id: JobId,
        /// Title for the jobs view ([`crate::JobSpec::title`]).
        title: String,
        /// How it is scheduled.
        mode: RunMode,
    },
    /// Job started running.
    Started {
        /// Job.
        id: JobId,
    },
    /// Job paused/resumed.
    StateChanged {
        /// Job.
        id: JobId,
        /// New state (`Paused` or `Running`).
        state: JobState,
    },
    /// Progress update (<= 10 Hz per job; last one before `Finished` is exact).
    Progress {
        /// Job.
        id: JobId,
        /// Snapshot.
        progress: Progress,
    },
    /// The worker is blocked until `reply` is answered.
    Ask {
        /// Job.
        id: JobId,
        /// Question.
        question: Question,
        /// Answer exactly once. Dropping it aborts the job.
        reply: Responder<Answer>,
    },
    /// Job ended (always the last event of a job).
    Finished {
        /// Job.
        id: JobId,
        /// Result.
        outcome: Outcome,
    },
}

/// Convenience for the UI: the name part of a path as `OsString` (cursor-on-new-item after an op).
pub fn created_name(path: &std::path::Path) -> Option<OsString> {
    path.file_name().map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overwrite_is_refused_for_mismatch_and_same_file() {
        for kind in [ConflictKind::TypeMismatch, ConflictKind::SameFile] {
            assert!(!kind.allows(ConflictAnswer::Overwrite));
            assert!(!kind.allows(ConflictAnswer::OverwriteIfNewer));
            assert!(kind.allows(ConflictAnswer::RenameNew));
            assert!(kind.allows(ConflictAnswer::Skip));
        }
        assert!(ConflictKind::Replaceable.allows(ConflictAnswer::Overwrite));
    }

    #[test]
    fn outcome_helpers() {
        assert!(Outcome::Ok(Report::default()).is_success());
        assert!(!Outcome::Cancelled(Report::default()).is_success());
        assert!(Outcome::Size(SizeTotals::default()).report().is_none());
        assert_eq!(
            created_name(std::path::Path::new("/a/b")),
            Some(OsString::from("b"))
        );
    }
}
