//! What a job does: the request types the UI submits (frozen by T3.0, ADR-0006).

use std::path::{Path, PathBuf};

use ofm_core::Location;

/// Identifier of a submitted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// How a job is scheduled (ADR-0006: one FIFO lane + any number of parallel jobs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    /// Runs on the single queue lane, after earlier queued jobs (default: one disk-heavy job at a
    /// time).
    #[default]
    Queued,
    /// Runs immediately on its own thread, independent of the lane.
    Parallel,
}

/// Options for copy and move.
#[derive(Debug, Clone, Default)]
pub struct CopyOpts {
    /// Rename mask applied to top-level source names, parsed with `ofm_core::mask::RenameMask`
    /// (`*.bak`, `backup_*`); a mask without wildcards is a literal new name (copy/rename a
    /// single item under another name, "rename in place" = `Move` into the same directory).
    /// `None` = keep names. An invalid mask fails the job with `JobError::Invalid` before any I/O.
    pub rename_mask: Option<String>,
    /// Copy what symlinks point to instead of recreating the links.
    pub follow_symlinks: bool,
    /// fsync every file and directory (always on for cross-device move).
    pub sync: bool,
    /// Pre-decided conflict policy (as if the user had answered once with "apply to all");
    /// `None` = ask via `JobEvent::Ask`.
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

/// A job request. Paths are absolute (the UI resolves relative input before submitting).
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
    /// Delete paths. Trash first when `to_trash`; if the trash is unavailable for a path the
    /// engine asks (`Question::TrashUnavailable`) before deleting it permanently.
    Delete {
        /// Paths.
        paths: Vec<PathBuf>,
        /// Move to trash instead of unlinking.
        to_trash: bool,
    },
    /// Create a directory (with parents, like `mkdir -p`; an existing directory is success).
    Mkdir {
        /// Directory to create.
        path: PathBuf,
    },
    /// Create a link.
    Link {
        /// Target (what the link points at); stored verbatim for symlinks (the UI computes
        /// relative targets with `ops::link::relative_target`).
        target: PathBuf,
        /// Link path to create.
        link: PathBuf,
        /// Symbolic or hard.
        kind: LinkKind,
    },
    /// chmod / chown (never follows symlinks; symlinks themselves are skipped for chmod).
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
    /// Recursive size calculation (result in `Outcome::Size`); read-only, cancellable.
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

/// Short display name of a location (last component for local paths).
fn loc_name(loc: &Location) -> String {
    match loc {
        Location::Local(p) => path_name(p),
        other => other.display_lossy(),
    }
}

fn path_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// `'name'` for one item, `N items` otherwise.
fn what(names: &[String]) -> String {
    match names {
        [one] => format!("'{one}'"),
        many => format!("{} items", many.len()),
    }
}

impl JobSpec {
    /// English title for the jobs view and progress dialogs, e.g. `Copy 3 items to /srv/backup`.
    /// Never touches the file system.
    pub fn title(&self) -> String {
        let locs = |v: &[Location]| what(&v.iter().map(loc_name).collect::<Vec<_>>());
        let paths = |v: &[PathBuf]| what(&v.iter().map(|p| path_name(p)).collect::<Vec<_>>());
        match self {
            JobSpec::Copy {
                sources, dest_dir, ..
            } => format!("Copy {} to {}", locs(sources), dest_dir.display()),
            JobSpec::Move {
                sources, dest_dir, ..
            } => format!("Move {} to {}", locs(sources), dest_dir.display()),
            JobSpec::Delete { paths: p, to_trash } if *to_trash => {
                format!("Move {} to Trash", paths(p))
            }
            JobSpec::Delete { paths: p, .. } => format!("Delete {}", paths(p)),
            JobSpec::Mkdir { path } => format!("Create directory '{}'", path_name(path)),
            JobSpec::Link { link, kind, .. } => match kind {
                LinkKind::Symbolic => format!("Create symbolic link '{}'", path_name(link)),
                LinkKind::Hard => format!("Create hard link '{}'", path_name(link)),
            },
            JobSpec::SetAttrs { paths: p, .. } => format!("Change attributes of {}", paths(p)),
            JobSpec::CalcSize { paths: p } => format!("Calculate size of {}", paths(p)),
            JobSpec::Extract {
                archive, dest_dir, ..
            } => format!("Extract '{}' to {}", path_name(archive), dest_dir.display()),
            JobSpec::Rename { pairs } => format!("Rename {} items", pairs.len()),
        }
    }

    /// Whether the job only reads (nothing to refresh in panels afterwards).
    pub fn is_read_only(&self) -> bool {
        matches!(self, JobSpec::CalcSize { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(p: &str) -> Location {
        Location::Local(p.into())
    }

    #[test]
    fn titles_are_short_and_lossy_safe() {
        let c = JobSpec::Copy {
            sources: vec![local("/a/b.txt")],
            dest_dir: "/dst".into(),
            opts: CopyOpts::default(),
        };
        assert_eq!(c.title(), "Copy 'b.txt' to /dst");
        let m = JobSpec::Move {
            sources: vec![local("/a/1"), local("/a/2")],
            dest_dir: "/dst".into(),
            opts: CopyOpts::default(),
        };
        assert_eq!(m.title(), "Move 2 items to /dst");
        let d = JobSpec::Delete {
            paths: vec!["/x/y".into()],
            to_trash: true,
        };
        assert_eq!(d.title(), "Move 'y' to Trash");
        assert!(!d.is_read_only());
        assert!(
            JobSpec::CalcSize {
                paths: vec!["/x".into()]
            }
            .is_read_only()
        );
    }
}
