//! ofm-jobs: background job engine and file operations (data-safety critical; ADR-0004, ADR-0006).
//! No GTK. The public API was frozen by T3.0; read ADR-0006 before changing it.
//!
//! File ownership for the M3 parallel tasks:
//! - frozen API (architect/lead only): `lib.rs`, `spec.rs`, `event.rs`, `error.rs`, `ctx.rs`,
//!   `runner.rs`, `fs/mod.rs`, `fs/faulty.rs`, `ops/mod.rs`
//! - T3.1: `engine.rs`, `plan.rs`, `copy.rs`, `move_.rs`, `progress.rs`, `fs/real.rs`
//! - T3.2: `ops/{delete,mkdir,link,attrs,size,trash}.rs` (+ `trash` dependency in `Cargo.toml`)
//! - T3.4: `tests/**`
//! - T6.2: `extract.rs` (frozen request types: `spec::ExtractOpts`, `JobSpec::Extract`)
//! - later: `ops/rename.rs`
//!
//! Layering: `engine` (threads, queue, events) -> `runner::run_job` -> operation (`copy`, `move_`,
//! `ops::*`) -> `JobCtx` (checkpoint/progress/questions) -> `Fs`. Operations never see the engine,
//! so they are unit-testable with `ctx::ScriptedSink` and `fs::faulty::FaultyFs`.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod copy;
pub mod ctx;
pub mod engine;
pub mod error;
pub mod event;
pub mod extract;
pub mod fs;
pub mod move_;
pub mod ops;
pub mod plan;
pub mod progress;
pub mod runner;
pub mod spec;

pub use ctx::{
    ErrorResolution, JobCtx, JobSink, PauseGate, Recording, Resolution, ScriptedSink,
    ask_over_events,
};
pub use engine::{JobEngine, JobInfo};
pub use error::JobError;
pub use event::{
    Answer, ConflictAnswer, ConflictKind, ErrorAction, ItemInfo, ItemKind, JobEvent, JobState,
    Operation, Outcome, Progress, Question, Report, SizeTotals, Warning,
};
pub use fs::{FileType, Fs, FsFile, FsStat, RealFs, TrashError};
pub use progress::{Clock, ManualClock, ProgressTracker, SystemClock};
pub use runner::{run_job, run_sync};
pub use spec::{CopyOpts, ExtractOpts, JobId, JobSpec, LinkKind, ModeChange, OwnerChange, RunMode};
