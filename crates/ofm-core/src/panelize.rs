//! "Panelize command output" (F25, ADR-0009): run a shell command on a worker thread, parse its
//! output as paths. Frozen by T7.0; the runner is implemented by T7.2.
//!
//! This module only produces *paths*. Turning them into panel rows (lstat, dropping paths that do
//! not exist) is `ofm_vfs::list::PanelList::from_paths`, and the panel itself shows the result as a
//! `Location::List`.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::cancel::CancelToken;

/// Paths per `PanelizeEvent::Paths` batch.
pub const BATCH_SIZE: usize = 1024;
/// Hard cap on parsed paths; reaching it stops the command and sets `PanelizeSummary::truncated`.
pub const MAX_PATHS: usize = 1_000_000;
/// How much of the command's stderr is kept for the error message.
pub const STDERR_TAIL_BYTES: usize = 4096;
/// Items longer than this are dropped (with a `PanelizeEvent::Warning`); no real path can be
/// longer than `PATH_MAX` anyway.
pub const MAX_ITEM_BYTES: usize = 4096;
/// Poll interval while waiting for command output (bounds cancel/timeout reaction time).
const POLL_TICK: Duration = Duration::from_millis(25);
/// How long `SIGTERM` gets to end the process group before `SIGKILL` follows.
const KILL_GRACE: Duration = Duration::from_millis(150);

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
    /// Something was skipped: over-long output items. Added by T7.2 (the frozen T7.0 enum had
    /// no warning carrier); the UI shows it like a search warning.
    Warning(String),
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
/// process group is killed and a `Done` with `cancelled = true` is the last event.
pub fn run_panelize(
    spec: &PanelizeSpec,
    cancel: &CancelToken,
    sink: &mut dyn FnMut(PanelizeEvent),
) -> Result<(), PanelizeError> {
    run_panelize_timed(spec, cancel, None, sink)
}

/// [`run_panelize`] with a wall-clock limit: when `timeout` elapses the process group is killed
/// like a cancel. The frozen [`PanelizeSummary`] has no timeout flag, so a timeout is reported as
/// `cancelled = true` (see the ADR-0009 notes).
pub fn run_panelize_timed(
    spec: &PanelizeSpec,
    cancel: &CancelToken,
    timeout: Option<Duration>,
    sink: &mut dyn FnMut(PanelizeEvent),
) -> Result<(), PanelizeError> {
    let shell = spec
        .shell
        .clone()
        .or_else(|| std::env::var_os("SHELL").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let mut child = Command::new(&shell)
        .arg("-c")
        // The command line is exactly one argv element after `-c`; it is never concatenated
        // with file names.
        .arg(&spec.command)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group: the child (and everything it spawns) can be killed as a unit and
        // never receives signals meant for OFM's own group.
        .process_group(0)
        .spawn()
        .map_err(|source| PanelizeError::Spawn {
            command: spec.command.clone(),
            source,
        })?;
    // `process_group(0)` makes the child a group leader, so pgid == pid.
    let pgid = child.id() as i32;

    // Drain stderr on a second thread so a chatty command can never fill the pipe and deadlock.
    let mut stderr = child.stderr.take().ok_or_else(|| {
        PanelizeError::Io(std::io::Error::other(
            "stderr not piped (by construction it is)",
        ))
    })?;
    let stderr_tail = std::thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::with_capacity(STDERR_TAIL_BYTES);
        let mut buf = [0u8; 8192];
        loop {
            match stderr.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > STDERR_TAIL_BYTES {
                        tail.drain(..tail.len() - STDERR_TAIL_BYTES);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        tail
    });

    // Read stdout on a third thread; the main loop selects between output and the cancel/timeout
    // checks (a plain blocking read could not react to cancel while the command is silent).
    let mut stdout = child.stdout.take().ok_or_else(|| {
        PanelizeError::Io(std::io::Error::other(
            "stdout not piped (by construction it is)",
        ))
    })?;
    let (tx, rx) = std_mpsc::channel::<Result<Vec<u8>, std::io::Error>>();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break; // consumer gone (cancel/timeout/truncation)
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });

    let mut summary = PanelizeSummary::default();
    let mut parser = PathParser::new(&spec.cwd, spec.delimiter);
    let mut batch: Vec<PathBuf> = Vec::with_capacity(BATCH_SIZE);
    let emit_paths = |batch: &mut Vec<PathBuf>, sink: &mut dyn FnMut(PanelizeEvent)| {
        if !batch.is_empty() {
            sink(PanelizeEvent::Paths(std::mem::take(batch)));
        }
    };
    let mut killed = false;
    let started = Instant::now();

    loop {
        if cancel.is_cancelled() {
            debug!("panelize cancelled by caller");
            summary.cancelled = true;
            killed = true;
            break;
        }
        if timeout.is_some_and(|t| started.elapsed() >= t) {
            debug!("panelize timed out");
            // Frozen summary has no timeout flag: report as cancelled.
            summary.cancelled = true;
            killed = true;
            break;
        }
        match rx.recv_timeout(POLL_TICK) {
            Ok(Ok(chunk)) => {
                let mut parsed = Vec::new();
                parser.feed(&chunk, &mut parsed);
                for path in parsed {
                    if summary.total_paths >= MAX_PATHS {
                        warn!(
                            total = summary.total_paths,
                            "panelize path cap reached; killing command"
                        );
                        summary.truncated = true;
                        killed = true;
                        break;
                    }
                    batch.push(path);
                    summary.total_paths += 1;
                    if batch.len() >= BATCH_SIZE {
                        emit_paths(&mut batch, sink);
                    }
                }
                if killed {
                    break;
                }
            }
            Ok(Err(e)) => {
                if !summary.cancelled {
                    kill_group(pgid);
                    drop(rx);
                    let _ = reader.join();
                    let _ = stderr_tail.join();
                    return Err(PanelizeError::Io(e));
                }
                break;
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // A trailing item without a final delimiter is still a path (matches `parse_paths`).
    let mut parsed = Vec::new();
    parser.finish(&mut parsed);
    for path in parsed {
        if summary.total_paths < MAX_PATHS {
            batch.push(path);
            summary.total_paths += 1;
        } else {
            summary.truncated = true;
        }
    }
    let dropped_long = parser.dropped_long;
    drop(parser);
    if dropped_long > 0 {
        sink(PanelizeEvent::Warning(format!(
            "{dropped_long} over-long output item(s) (> {MAX_ITEM_BYTES} bytes) dropped"
        )));
    }
    emit_paths(&mut batch, sink);

    if killed {
        kill_group(pgid);
    }
    drop(rx);
    let _ = reader.join();
    // Reap the child; std caches the status, so this also works after the graceful TERM phase of
    // `kill_group` already reaped it.
    summary.exit_code = child.wait().ok().and_then(|status| status.code());
    summary.stderr_tail =
        String::from_utf8_lossy(&stderr_tail.join().unwrap_or_default()).into_owned();
    sink(PanelizeEvent::Done(summary));
    Ok(())
}

/// Kill the whole process group: `SIGTERM`, up to `KILL_GRACE` to exit, then `SIGKILL`.
fn kill_group(pgid: i32) {
    // SAFETY: killpg with a positive pgid sends a signal to that group; the group was created by
    // us via `process_group(0)` and only our command's processes belong to it. ESRCH is fine
    // (already gone).
    unsafe {
        libc::killpg(pgid, libc::SIGTERM);
    }
    let grace_deadline = Instant::now() + KILL_GRACE;
    loop {
        // Poll /proc-less: killpg 0 probes for existence of the group.
        let alive = unsafe { libc::killpg(pgid, 0) } == 0;
        if !alive || Instant::now() >= grace_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

/// Incremental splitter turning command output into paths, mirroring [`parse_paths`] semantics
/// across arbitrary chunk boundaries.
struct PathParser<'a> {
    cwd: &'a Path,
    delim: Delimiter,
    /// Bytes not yet terminated by the delimiter.
    carry: Vec<u8>,
    /// Currently discarding an over-long item.
    discarding: bool,
    /// Over-long items dropped so far.
    dropped_long: usize,
}

impl<'a> PathParser<'a> {
    fn new(cwd: &'a Path, delim: Delimiter) -> Self {
        PathParser {
            cwd,
            delim,
            carry: Vec::new(),
            discarding: false,
            dropped_long: 0,
        }
    }

    fn sep(&self) -> u8 {
        match self.delim {
            Delimiter::Newline => b'\n',
            Delimiter::Nul => 0,
        }
    }

    fn feed(&mut self, chunk: &[u8], out: &mut Vec<PathBuf>) {
        self.carry.extend_from_slice(chunk);
        self.drain(out);
    }

    /// Parse every complete item out of `carry`.
    fn drain(&mut self, out: &mut Vec<PathBuf>) {
        let sep = self.sep();
        loop {
            let Some(i) = self.carry.iter().position(|b| *b == sep) else {
                if !self.discarding && self.carry.len() > MAX_ITEM_BYTES {
                    // No end in sight and already longer than any real path: drop it.
                    self.carry.clear();
                    self.discarding = true;
                }
                break;
            };
            let item: Vec<u8> = self.carry.drain(..=i).collect();
            if self.discarding {
                self.discarding = false;
                self.dropped_long += 1;
            } else if item.len() - 1 > MAX_ITEM_BYTES {
                self.dropped_long += 1;
            } else if let Some(path) = item_to_path(self.cwd, self.delim, &item[..item.len() - 1]) {
                out.push(path);
            }
        }
    }

    /// Stream ended: a remaining unterminated item is a path, unless it was over-long.
    fn finish(&mut self, out: &mut Vec<PathBuf>) {
        if self.discarding || self.carry.len() > MAX_ITEM_BYTES {
            self.dropped_long += 1;
        } else if let Some(path) = item_to_path(self.cwd, self.delim, &self.carry) {
            out.push(path);
        }
        self.carry.clear();
    }
}

/// One output item (delimiter already stripped) to a path: drop a trailing `\r` in newline mode,
/// skip empty items, join relative items to `cwd`, drop `.` components. Bytes, not `String`.
fn item_to_path(cwd: &Path, delim: Delimiter, item: &[u8]) -> Option<PathBuf> {
    let item = match (delim, item.last()) {
        (Delimiter::Newline, Some(b'\r')) => &item[..item.len() - 1],
        _ => item,
    };
    if item.is_empty() {
        return None;
    }
    let p = Path::new(OsStr::from_bytes(item));
    let joined = if p.is_absolute() {
        p.to_owned()
    } else {
        cwd.join(p)
    };
    Some(
        joined
            .components()
            .filter(|c| !matches!(c, Component::CurDir))
            .collect(),
    )
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
        .filter_map(|item| item_to_path(cwd, delim, item))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
    fn incremental_parser_matches_parse_paths_across_boundaries() {
        let cwd = Path::new("/w");
        for (delim, data) in [
            (Delimiter::Newline, b"./a b\r\n\xffx\nrel/c".to_vec()),
            (Delimiter::Nul, b"a\nb\0\xff\0./c\0".to_vec()),
        ] {
            let mut parser = PathParser::new(cwd, delim);
            let mut got = Vec::new();
            for chunk in data.chunks(3) {
                parser.feed(chunk, &mut got);
            }
            parser.finish(&mut got);
            assert_eq!(got, parse_paths(cwd, &data, delim));
        }
    }

    #[test]
    fn parser_drops_over_long_items() {
        let cwd = Path::new("/w");
        let mut parser = PathParser::new(cwd, Delimiter::Newline);
        let mut got = Vec::new();
        // 5000 'x' with no newline: over the cap, then the delimiter ends the item.
        let mut chunk = vec![b'x'; 5000];
        chunk.push(b'\n');
        chunk.extend_from_slice(b"ok\n");
        parser.feed(&chunk, &mut got);
        parser.finish(&mut got);
        assert_eq!(got, [PathBuf::from("/w/ok")]);
        assert_eq!(parser.dropped_long, 1);
        // A long item split across chunks is caught too.
        let mut parser = PathParser::new(cwd, Delimiter::Newline);
        let mut got = Vec::new();
        parser.feed(&[b'y'; 3000], &mut got);
        parser.feed(&[b'y'; 3000], &mut got);
        parser.feed(b"\n", &mut got);
        parser.finish(&mut got);
        assert!(got.is_empty());
        assert_eq!(parser.dropped_long, 1);
    }

    #[test]
    fn runner_streams_paths_and_summary() {
        let dir = std::env::temp_dir();
        let spec = PanelizeSpec {
            cwd: dir.clone(),
            command: "printf 'a\\n./b\\n\\n'".into(),
            shell: Some("/bin/sh".into()),
            delimiter: Delimiter::Newline,
        };
        let mut events = Vec::new();
        run_panelize(&spec, &CancelToken::new(), &mut |ev| events.push(ev)).unwrap();
        let paths: Vec<PathBuf> = events
            .iter()
            .filter_map(|ev| match ev {
                PanelizeEvent::Paths(p) => Some(p.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(paths, [dir.join("a"), dir.join("b")]);
        let done = events
            .iter()
            .find_map(|ev| match ev {
                PanelizeEvent::Done(s) => Some(s.clone()),
                _ => None,
            })
            .expect("a Done event");
        assert_eq!(done.exit_code, Some(0));
        assert!(!done.cancelled && !done.truncated);
        assert!(matches!(events.last(), Some(PanelizeEvent::Done(_))));
    }

    #[test]
    fn nul_mode_streams_through_runner() {
        let dir = std::env::temp_dir();
        let spec = PanelizeSpec {
            cwd: dir.clone(),
            command: "printf 'a\\nb\\0c\\0'".into(),
            shell: Some("/bin/sh".into()),
            delimiter: Delimiter::Nul,
        };
        let mut events = Vec::new();
        run_panelize(&spec, &CancelToken::new(), &mut |ev| events.push(ev)).unwrap();
        let paths: Vec<PathBuf> = events
            .iter()
            .filter_map(|ev| match ev {
                PanelizeEvent::Paths(p) => Some(p.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(paths, [dir.join("a\nb"), dir.join("c")]);
    }
}
