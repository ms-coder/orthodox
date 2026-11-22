//! "Panelize command output" (F25, ADR-0009): run a shell command on a worker thread, parse its
//! output as paths. Frozen by T7.0; the runner is implemented by T7.2.
//!
//! This module only produces *paths*. Turning them into panel rows (lstat, dropping paths that do
//! not exist) is `ofm_vfs::list::PanelList::from_paths`, and the panel itself shows the result as a
//! `Location::List`.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use crate::cancel::CancelToken;

/// Paths per `PanelizeEvent::Paths` batch.
pub const BATCH_SIZE: usize = 1024;
/// Hard cap on parsed paths; reaching it stops the command and sets `PanelizeSummary::truncated`.
pub const MAX_PATHS: usize = 1_000_000;
/// How much of the command's stderr is kept for the error message.
pub const STDERR_TAIL_BYTES: usize = 4096;

/// How the command separates paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delimiter {
    /// One path per line (`\n`; a trailing `\r` is dropped).
    #[default]
    Newline,
    /// NUL separated (`find -print0`, `git ls-files -z`): file names may contain newlines.
    Nul,
}

/// What to run.
#[derive(Debug, Clone)]
pub struct PanelizeSpec {
    /// Working directory of the command and base for relative output lines (the panel's directory).
    pub cwd: PathBuf,
    /// Command line, run as `<shell> -c <command>` with stdin closed.
    pub command: String,
    /// Shell; `None` = `$SHELL`, else `/bin/sh`.
    pub shell: Option<PathBuf>,
    /// Output delimiter.
    pub delimiter: Delimiter,
}

/// End-of-run information (the paths already went out as `Paths` events).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanelizeSummary {
    /// Paths emitted (before the caller drops non-existing ones).
    pub total_paths: usize,
    /// Exit code, `None` if killed by a signal or cancelled. A non-zero code is NOT an error
    /// (`grep -l` exits 1 without matches, `find` 1 after a permission error): the UI shows
    /// `stderr_tail` as a warning when it is non-empty.
    pub exit_code: Option<i32>,
    /// Last `STDERR_TAIL_BYTES` of stderr, lossy.
    pub stderr_tail: String,
    /// `MAX_PATHS` reached, the command was stopped.
    pub truncated: bool,
    /// Stopped by the `CancelToken`.
    pub cancelled: bool,
}

/// Events of one run, in order: `Paths`*, then exactly one `Done`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelizeEvent {
    /// A batch (<= `BATCH_SIZE`) of absolute or cwd-joined paths, in output order, not checked for
    /// existence.
    Paths(Vec<PathBuf>),
    /// Finished (also after cancel).
    Done(PanelizeSummary),
}

/// Failure to run the command at all.
#[derive(Debug, thiserror::Error)]
pub enum PanelizeError {
    /// The shell could not be started (missing shell, cwd gone, permission).
    #[error("cannot run {command:?}: {source}")]
    Spawn {
        /// The command line.
        command: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// Reading the output failed.
    #[error("reading command output: {0}")]
    Io(#[source] std::io::Error),
    /// Not implemented yet (stub of T7.2).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Run `spec.command` in `spec.cwd`, streaming parsed paths into `sink`; blocks until the command
/// ends, so call it on a worker thread (the app wraps it in a job-like task). On `cancel` the
/// process group is killed and a `Done` with `cancelled = true` is the last event. Implementation
/// notes (T7.2): `Command::new(shell).arg("-c")`, own process group, stdout read in chunks split
/// with [`parse_paths`] semantics across chunk boundaries, stderr drained on a second thread.
pub fn run_panelize(
    _spec: &PanelizeSpec,
    _cancel: &CancelToken,
    _sink: &mut dyn FnMut(PanelizeEvent),
) -> Result<(), PanelizeError> {
    Err(PanelizeError::NotImplemented("run_panelize (T7.2)"))
}

/// Parse complete command output into paths: split at `delim`, drop empty items, drop a trailing
/// `\r` in newline mode, join relative items to `cwd`, remove `.` components. `..` is kept (it may
/// cross symlinks); existence is not checked.
pub fn parse_paths(cwd: &Path, output: &[u8], delim: Delimiter) -> Vec<PathBuf> {
    let sep = match delim {
        Delimiter::Newline => b'\n',
        Delimiter::Nul => 0,
    };
    output
        .split(|b| *b == sep)
        .map(|item| match (delim, item.last()) {
            (Delimiter::Newline, Some(b'\r')) => &item[..item.len() - 1],
            _ => item,
        })
        .filter(|item| !item.is_empty())
        .map(|item| {
            let p = Path::new(OsStr::from_bytes(item));
            let joined = if p.is_absolute() {
                p.to_owned()
            } else {
                cwd.join(p)
            };
            joined
                .components()
                .filter(|c| !matches!(c, Component::CurDir))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_relative_to_cwd() {
        let got = parse_paths(
            Path::new("/work"),
            b"a.txt\n./sub/b.txt\n/abs/c\n\n\nd e\r\n",
            Delimiter::Newline,
        );
        let want: Vec<PathBuf> = ["/work/a.txt", "/work/sub/b.txt", "/abs/c", "/work/d e"]
            .map(PathBuf::from)
            .into();
        assert_eq!(got, want);
    }

    #[test]
    fn nul_mode_allows_newlines_and_keeps_bytes() {
        let got = parse_paths(Path::new("/w"), b"a\nb\0\xff\xfe\0", Delimiter::Nul);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], Path::new("/w/a\nb"));
        assert_eq!(got[1].as_os_str().as_bytes(), b"/w/\xff\xfe");
    }

    #[test]
    fn runner_is_a_typed_stub_until_t72() {
        let spec = PanelizeSpec {
            cwd: "/".into(),
            command: "true".into(),
            shell: None,
            delimiter: Delimiter::Newline,
        };
        let r = run_panelize(&spec, &CancelToken::new(), &mut |_| {});
        assert!(matches!(r, Err(PanelizeError::NotImplemented(_))));
    }
}
