//! ofm-jobs: background job engine and file operations (data-safety critical; ADR-0004).
//! No GTK. These are the T1.0 stubs; the M3 architect task (T3.0) freezes the final API before the
//! engine is implemented, so treat details here as provisional except the names in ARCHITECTURE §2.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod engine;
pub mod error;
pub mod event;
pub mod fs;
pub mod spec;

pub use engine::JobEngine;
pub use error::JobError;
pub use event::{Answer, ConflictAnswer, ErrorAction, JobEvent, Outcome, Progress, Question};
pub use fs::{Fs, FsFile, FsStat, RealFs};
pub use spec::{CopyOpts, JobId, JobSpec, LinkKind, ModeChange, OwnerChange, RunMode};
