//! Fault-injection plans for the job engine's `FaultyFs` (`ofm_jobs::fs::faulty`, feature `testing`).
//!
//! Plain data only: a [`FaultPlan`] describes *which* operation fails *when*
//! and *how*. The `FaultyFs` wrapper (to be implemented in `ofm-jobs` around
//! its `Fs` trait, per ADR-0004) delegates every call to the real filesystem,
//! consults the plan through [`FaultState`] and applies the returned effect.
//! This crate intentionally does not depend on `ofm-jobs` — the plan travels
//! in the opposite direction (dev-dependency).
//!
//! # Example
//!
//! ```
//! use ofm_testkit::fault::{FaultErrno, FaultEffect, FaultOp, FaultPlan, FaultState};
//!
//! let plan = FaultPlan::new()
//!     .fail_nth(FaultOp::CreateExcl, 3, FaultErrno::NoSpace)
//!     .short_write_at(2, 4096);
//!
//! let mut state = FaultState::new(plan);
//! // first two creates succeed, the third fails with ENOSPC:
//! assert!(state.next(FaultOp::CreateExcl).is_none());
//! assert!(state.next(FaultOp::CreateExcl).is_none());
//! assert_eq!(
//!     state.next(FaultOp::CreateExcl),
//!     Some(FaultEffect::Error(FaultErrno::NoSpace))
//! );
//! // the second write reports only 4096 bytes written, however many were
//! // asked for — the engine's write loop must cope:
//! assert_eq!(state.next(FaultOp::FileWrite), None);
//! assert_eq!(
//!     state.next(FaultOp::FileWrite),
//!     Some(FaultEffect::ShortWrite { bytes: 4096 })
//! );
//! ```

use std::collections::HashMap;
use std::io;

/// Which `Fs`/`FsFile` operation a fault applies to. Variant names mirror the
/// methods of `ofm_jobs::fs` (`File*` = handle methods). Pure queries
/// (`is_root`) are not faultable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultOp {
    /// `Fs::open_read`.
    OpenRead,
    /// `Fs::create_excl`.
    CreateExcl,
    /// `Fs::stat`.
    Stat,
    /// `Fs::lstat`.
    Lstat,
    /// `Fs::read_dir`.
    ReadDir,
    /// `Fs::mkdir`.
    Mkdir,
    /// `Fs::rename_noreplace`.
    RenameNoreplace,
    /// `Fs::rename_replace`.
    RenameReplace,
    /// `Fs::unlink`.
    Unlink,
    /// `Fs::rmdir`.
    Rmdir,
    /// `Fs::symlink`.
    Symlink,
    /// `Fs::hard_link`.
    HardLink,
    /// `Fs::read_link`.
    ReadLink,
    /// `Fs::set_mode` (path).
    SetMode,
    /// `Fs::set_times` (path).
    SetTimes,
    /// `Fs::set_owner` (path).
    SetOwner,
    /// `Fs::sync_dir`.
    SyncDir,
    /// `Fs::list_xattrs`.
    ListXattrs,
    /// `Fs::get_xattr`.
    GetXattr,
    /// `Fs::set_xattr`.
    SetXattr,
    /// `Fs::remove_xattr`.
    RemoveXattr,
    /// `Fs::trash`.
    Trash,
    /// `Fs::pin` (directory handles of anchored access).
    Pin,
    /// `FsFile::stat`.
    FileStat,
    /// `FsFile::read`.
    FileRead,
    /// `FsFile::write`.
    FileWrite,
    /// `FsFile::try_clone_from`.
    TryCloneFrom,
    /// `FsFile::copy_range_from`.
    CopyRangeFrom,
    /// `FsFile::seek_data`.
    SeekData,
    /// `FsFile::seek_hole`.
    SeekHole,
    /// `FsFile::seek`.
    Seek,
    /// `FsFile::set_len`.
    SetLen,
    /// `FsFile::set_mode` (handle).
    FileSetMode,
    /// `FsFile::set_times` (handle).
    FileSetTimes,
    /// `FsFile::set_owner` (handle).
    FileSetOwner,
    /// `FsFile::sync`.
    FileSync,
    /// `FsFile::close` (checked close: deferred write errors of NFS/FUSE show up here).
    FileClose,
}

/// When a fault fires; calls are counted per [`FaultOp`], 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occurrence {
    /// Only the n-th call (`Nth(1)` = the very first).
    Nth(u64),
    /// The n-th call and every call after it (persistent failure).
    FromNth(u64),
}

fn occurrence_matches(at: Occurrence, call: u64) -> bool {
    match at {
        Occurrence::Nth(n) => call == n,
        Occurrence::FromNth(n) => call >= n,
    }
}

/// OS errors that can be injected without a libc dependency. The numbers are
/// Linux errno values (this project targets Linux; see `to_io_error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultErrno {
    /// `ENOSPC` (28): no space left on device.
    NoSpace,
    /// `EIO` (5): I/O error.
    Io,
    /// `EACCES` (13): permission denied.
    PermissionDenied,
    /// `ENOENT` (2): no such file or directory.
    NotFound,
    /// Any other errno, by number.
    Raw(i32),
}

impl FaultErrno {
    /// The error the `FaultyFs` should return for this fault.
    pub fn to_io_error(self) -> io::Error {
        let n = match self {
            FaultErrno::NoSpace => 28,          // ENOSPC
            FaultErrno::Io => 5,                // EIO
            FaultErrno::PermissionDenied => 13, // EACCES
            FaultErrno::NotFound => 2,          // ENOENT
            FaultErrno::Raw(n) => n,
        };
        io::Error::from_raw_os_error(n)
    }
}

/// What happens when a fault fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultEffect {
    /// The call fails with this OS error.
    Error(FaultErrno),
    /// A write returns `Ok(bytes)` with `bytes < buf.len()` — a short write
    /// the engine's write loop must handle. Applies to `FaultOp::FileWrite`.
    ShortWrite {
        /// Number of bytes to report as written.
        bytes: usize,
    },
    /// The call is performed, but reports this error afterwards: the server did the work and the
    /// reply was lost (an NFS client retransmitting `link` then sees `EEXIST`). Applies to
    /// `FaultOp::HardLink`.
    LostReply(FaultErrno),
}

/// One injected fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    /// Operation to fault.
    pub op: FaultOp,
    /// When it fires.
    pub at: Occurrence,
    /// What happens.
    pub effect: FaultEffect,
}

/// An ordered set of faults. When several faults match a call, the FIRST one
/// in plan order wins — put specific faults before broad `FromNth` ones.
///
/// Build with the `fail_*` methods (each consumes and returns `self`) and let
/// the `FaultyFs` drive it via [`FaultState`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FaultPlan {
    faults: Vec<Fault>,
}

impl FaultPlan {
    /// Empty plan (no faults).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the plan contains no faults.
    pub fn is_empty(&self) -> bool {
        self.faults.is_empty()
    }

    /// All faults, in match order.
    pub fn faults(&self) -> &[Fault] {
        &self.faults
    }

    /// Fail the n-th call of `op` with `errno` (1-based).
    pub fn fail_nth(mut self, op: FaultOp, nth: u64, errno: FaultErrno) -> Self {
        self.faults.push(Fault {
            op,
            at: Occurrence::Nth(nth),
            effect: FaultEffect::Error(errno),
        });
        self
    }

    /// Fail every call of `op` from the n-th on (1-based) — persistent failure.
    pub fn fail_from(mut self, op: FaultOp, nth: u64, errno: FaultErrno) -> Self {
        self.faults.push(Fault {
            op,
            at: Occurrence::FromNth(nth),
            effect: FaultEffect::Error(errno),
        });
        self
    }

    /// Make the n-th `FsFile::write` call a short write of `bytes` (1-based).
    pub fn short_write_at(mut self, nth: u64, bytes: usize) -> Self {
        self.faults.push(Fault {
            op: FaultOp::FileWrite,
            at: Occurrence::Nth(nth),
            effect: FaultEffect::ShortWrite { bytes },
        });
        self
    }

    /// Perform the n-th call of `op` and then report `errno` all the same (1-based, see
    /// [`FaultEffect::LostReply`]).
    pub fn lose_reply_nth(mut self, op: FaultOp, nth: u64, errno: FaultErrno) -> Self {
        self.faults.push(Fault {
            op,
            at: Occurrence::Nth(nth),
            effect: FaultEffect::LostReply(errno),
        });
        self
    }

    /// Effect to apply to the `call`-th (1-based) invocation of `op`, if any.
    pub fn effect_for(&self, op: FaultOp, call: u64) -> Option<FaultEffect> {
        self.faults
            .iter()
            .find(|f| f.op == op && occurrence_matches(f.at, call))
            .map(|f| f.effect)
    }
}

/// Call counter over a [`FaultPlan`] — the state a `FaultyFs` holds.
///
/// Call counters are per operation and independent of each other.
#[derive(Debug, Clone)]
pub struct FaultState {
    plan: FaultPlan,
    calls: HashMap<FaultOp, u64>,
}

impl FaultState {
    /// State that will apply `plan`.
    pub fn new(plan: FaultPlan) -> Self {
        Self {
            plan,
            calls: HashMap::new(),
        }
    }

    /// Record one call of `op` and return the effect to apply, if any.
    pub fn next(&mut self, op: FaultOp) -> Option<FaultEffect> {
        let n = self.calls.entry(op).or_default();
        *n = n.saturating_add(1);
        self.plan.effect_for(op, *n)
    }

    /// How many calls of `op` were recorded so far (for assertions in tests).
    pub fn calls_of(&self, op: FaultOp) -> u64 {
        self.calls.get(&op).copied().unwrap_or(0)
    }

    /// The plan this state applies.
    pub fn plan(&self) -> &FaultPlan {
        &self.plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nth_fires_exactly_once() {
        let plan = FaultPlan::new().fail_nth(FaultOp::Unlink, 2, FaultErrno::Io);
        assert_eq!(plan.effect_for(FaultOp::Unlink, 1), None);
        assert_eq!(
            plan.effect_for(FaultOp::Unlink, 2),
            Some(FaultEffect::Error(FaultErrno::Io))
        );
        assert_eq!(plan.effect_for(FaultOp::Unlink, 3), None);
        // other ops unaffected
        assert_eq!(plan.effect_for(FaultOp::Rmdir, 2), None);
    }

    #[test]
    fn from_nth_is_persistent() {
        let plan = FaultPlan::new().fail_from(FaultOp::CreateExcl, 3, FaultErrno::NoSpace);
        assert_eq!(plan.effect_for(FaultOp::CreateExcl, 2), None);
        for call in 3..6 {
            assert_eq!(
                plan.effect_for(FaultOp::CreateExcl, call),
                Some(FaultEffect::Error(FaultErrno::NoSpace))
            );
        }
    }

    #[test]
    fn first_matching_fault_wins() {
        let plan = FaultPlan::new()
            .fail_nth(FaultOp::OpenRead, 2, FaultErrno::Io)
            .fail_from(FaultOp::OpenRead, 2, FaultErrno::NoSpace);
        assert_eq!(
            plan.effect_for(FaultOp::OpenRead, 2),
            Some(FaultEffect::Error(FaultErrno::Io))
        );
        assert_eq!(
            plan.effect_for(FaultOp::OpenRead, 3),
            Some(FaultEffect::Error(FaultErrno::NoSpace))
        );
    }

    #[test]
    fn lost_reply_fires_at_the_nth_call() {
        let plan = FaultPlan::new().lose_reply_nth(FaultOp::HardLink, 2, FaultErrno::Raw(17));
        assert_eq!(plan.effect_for(FaultOp::HardLink, 1), None);
        assert_eq!(
            plan.effect_for(FaultOp::HardLink, 2),
            Some(FaultEffect::LostReply(FaultErrno::Raw(17)))
        );
    }

    #[test]
    fn short_write_targets_writes() {
        let plan = FaultPlan::new().short_write_at(2, 4096);
        assert_eq!(plan.effect_for(FaultOp::FileWrite, 1), None);
        assert_eq!(
            plan.effect_for(FaultOp::FileWrite, 2),
            Some(FaultEffect::ShortWrite { bytes: 4096 })
        );
        assert_eq!(plan.effect_for(FaultOp::FileRead, 2), None);
    }

    #[test]
    fn state_counts_per_op() {
        let mut state =
            FaultState::new(FaultPlan::new().fail_nth(FaultOp::Mkdir, 2, FaultErrno::Io));
        assert_eq!(state.next(FaultOp::Mkdir), None);
        assert_eq!(state.calls_of(FaultOp::Mkdir), 1);
        assert_eq!(
            state.next(FaultOp::Mkdir),
            Some(FaultEffect::Error(FaultErrno::Io))
        );
        assert_eq!(state.next(FaultOp::Mkdir), None);
        assert_eq!(state.calls_of(FaultOp::Mkdir), 3);
        assert_eq!(state.calls_of(FaultOp::Unlink), 0);
    }

    #[test]
    fn errno_values() {
        assert_eq!(FaultErrno::NoSpace.to_io_error().raw_os_error(), Some(28));
        assert_eq!(FaultErrno::Io.to_io_error().raw_os_error(), Some(5));
        assert_eq!(
            FaultErrno::PermissionDenied.to_io_error().raw_os_error(),
            Some(13)
        );
        assert_eq!(FaultErrno::NotFound.to_io_error().raw_os_error(), Some(2));
        assert_eq!(FaultErrno::Raw(122).to_io_error().raw_os_error(), Some(122));
        assert_eq!(
            FaultErrno::NoSpace.to_io_error().kind(),
            std::io::ErrorKind::StorageFull
        );
    }
}
