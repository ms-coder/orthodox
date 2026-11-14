//! Events from workers to the UI, including questions the UI answers.

use std::path::PathBuf;

use ofm_core::{Entry, Responder};

use crate::spec::JobId;

/// Throttled (<= 10 Hz per job) progress snapshot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
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

/// Final result of a job.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Everything succeeded (possibly with warnings listed).
    Ok {
        /// Non-fatal warnings (e.g. xattr not copied, special file skipped).
        warnings: Vec<String>,
    },
    /// Cancelled by the user; no half-written destination files remain.
    Cancelled,
    /// Failed; short summary for the UI (details were reported through `Ask`/warnings).
    Failed(String),
    /// Result of `CalcSize`.
    Size {
        /// Total bytes.
        bytes: u64,
        /// Number of files.
        files: u64,
        /// Number of directories.
        dirs: u64,
    },
}

/// How to resolve a destination conflict (F16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAnswer {
    /// Replace the destination (atomic rename over it).
    Overwrite,
    /// Leave the destination, skip this item.
    Skip,
    /// Overwrite only if the source is newer.
    OverwriteIfNewer,
    /// Keep both: write the new file under a fresh name.
    RenameNew,
    /// Keep both: rename the existing destination out of the way.
    RenameExisting,
    /// Abort the whole job.
    Abort,
}

/// How to react to an error on one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAction {
    /// Try again.
    Retry,
    /// Skip this item and continue.
    Skip,
    /// Abort the job.
    Abort,
}

/// A question a worker asks while running.
#[derive(Debug, Clone)]
pub enum Question {
    /// Destination exists.
    Conflict {
        /// Source metadata.
        source: Entry,
        /// Source path (display).
        source_path: PathBuf,
        /// Existing destination metadata.
        existing: Entry,
        /// Destination path.
        dest_path: PathBuf,
    },
    /// An operation failed on `path`.
    Error {
        /// Path.
        path: PathBuf,
        /// What was being done ("copy", "delete", ...).
        operation: &'static str,
        /// OS/library message.
        message: String,
    },
    /// Trash is unavailable on this mount: delete permanently?
    TrashUnavailable {
        /// Path that cannot be trashed.
        path: PathBuf,
    },
}

/// The UI's answer, matching the [`Question`] variant. `apply_to_all` remembers the decision for
/// the rest of the job.
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
    /// To `Question::TrashUnavailable`: true = delete permanently.
    Confirm(bool),
}

/// Event stream item.
#[derive(Debug)]
pub enum JobEvent {
    /// Job started running.
    Started {
        /// Job.
        id: JobId,
        /// Title for the jobs view.
        title: String,
    },
    /// Progress update.
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
    /// Job ended.
    Finished {
        /// Job.
        id: JobId,
        /// Result.
        outcome: Outcome,
    },
}
