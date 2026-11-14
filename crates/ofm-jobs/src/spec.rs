//! What a job does: the request types the UI submits.

use std::path::PathBuf;

use ofm_core::Location;

/// Identifier of a submitted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// How a job is scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Runs on the single queue lane, after earlier queued jobs.
    Queued,
    /// Runs immediately on its own thread.
    Parallel,
}

/// Options for copy and move.
#[derive(Debug, Clone, Default)]
pub struct CopyOpts {
    /// Rename mask applied to top-level source names (`*.bak`); `None` = keep names.
    pub rename_mask: Option<String>,
    /// Copy what symlinks point to instead of recreating the links.
    pub follow_symlinks: bool,
    /// fsync every file and directory (always on for cross-device move).
    pub sync: bool,
    /// Pre-decided conflict policy; `None` = ask via `JobEvent::Ask`.
    pub on_conflict: Option<crate::event::ConflictAnswer>,
}

/// Kind of link to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// Symlink with the given target text as is (absolute or relative as the caller prepared it).
    Symbolic,
    /// Hard link.
    Hard,
}

/// chmod request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeChange {
    /// Bits to set.
    pub set: u32,
    /// Bits to clear.
    pub clear: u32,
}

/// chown/chgrp request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerChange {
    /// New uid, if changing.
    pub uid: Option<u32>,
    /// New gid, if changing.
    pub gid: Option<u32>,
}

/// A job request.
#[derive(Debug, Clone)]
pub enum JobSpec {
    /// Copy sources into `dest_dir`.
    Copy {
        /// Sources (archive/list entries are extracted by reading through `Vfs`).
        sources: Vec<Location>,
        /// Destination directory.
        dest_dir: PathBuf,
        /// Options.
        opts: CopyOpts,
    },
    /// Move sources into `dest_dir` (rename when on the same file system, else copy + delete).
    Move {
        /// Sources.
        sources: Vec<Location>,
        /// Destination directory.
        dest_dir: PathBuf,
        /// Options.
        opts: CopyOpts,
    },
    /// Delete paths.
    Delete {
        /// Paths.
        paths: Vec<PathBuf>,
        /// Move to trash instead of unlinking.
        to_trash: bool,
    },
    /// Create a directory (with parents).
    Mkdir {
        /// Directory to create.
        path: PathBuf,
    },
    /// Create a link.
    Link {
        /// Target (what the link points at).
        target: PathBuf,
        /// Link path to create.
        link: PathBuf,
        /// Symbolic or hard.
        kind: LinkKind,
    },
    /// chmod / chown.
    SetAttrs {
        /// Paths.
        paths: Vec<PathBuf>,
        /// Mode change.
        mode: Option<ModeChange>,
        /// Owner change.
        owner: Option<OwnerChange>,
        /// Apply recursively.
        recursive: bool,
    },
    /// Recursive size calculation (result in `Outcome::Size`).
    CalcSize {
        /// Paths.
        paths: Vec<PathBuf>,
    },
    /// Extract archive entries (v1.0: implemented as copy from the archive VFS).
    Extract {
        /// Archive file.
        archive: PathBuf,
        /// Inner paths; empty = everything.
        entries: Vec<PathBuf>,
        /// Destination directory.
        dest_dir: PathBuf,
    },
    /// Batch rename (F20): validated for conflicts/cycles by the caller; applied atomically per pair.
    Rename {
        /// `(from, to)` pairs.
        pairs: Vec<(PathBuf, PathBuf)>,
    },
}

impl JobSpec {
    /// Short English title for the jobs view.
    pub fn title(&self) -> String {
        match self {
            JobSpec::Copy { .. } => "Copy".into(),
            JobSpec::Move { .. } => "Move".into(),
            JobSpec::Delete { .. } => "Delete".into(),
            JobSpec::Mkdir { .. } => "Make directory".into(),
            JobSpec::Link { .. } => "Create link".into(),
            JobSpec::SetAttrs { .. } => "Change attributes".into(),
            JobSpec::CalcSize { .. } => "Calculate size".into(),
            JobSpec::Extract { .. } => "Extract".into(),
            JobSpec::Rename { .. } => "Rename".into(),
        }
    }
}
