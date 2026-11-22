//! Running a finished menu command (F28).
//!
//! Both ways run `/bin/sh -c <command>` with the exact command bytes (nothing is typed into the
//! interactive shell, so no line-editor tricks and no "shell must be idle" requirement):
//! - terminal entries go through `TerminalHandle::run_program` (own VTE view, panels come back
//!   when the program exits);
//! - the others run as a detached child on a dedicated thread (never the GTK thread, never an
//!   I/O pool thread: a long command must not starve directory listings). stdout and stderr go
//!   into bounded buffers; the outcome comes back over a channel.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ofm_core::cmdline::QuoteStyle;

/// How much of each output stream is kept (the tail).
pub const OUTPUT_CAP: usize = 16 * 1024;

/// Lines of output shown in a message.
pub const SHOWN_LINES: usize = 12;

/// The shell that runs every menu command: `sh` reads POSIX quoting, so this is also the
/// [`QuoteStyle`] used to expand the template — whatever the user's login shell or the embedded
/// terminal's shell is (fish, ...), because it is never the one that parses the command.
pub const SHELL: &str = "/bin/sh";

/// Quote style for names substituted into a command that `SHELL` will run.
pub fn quote_style() -> QuoteStyle {
    QuoteStyle::Posix
}

/// The exact argument vector for a command: `["/bin/sh", "-c", command]`.
pub fn argv(command: &[u8]) -> Vec<OsString> {
    vec![
        OsString::from(SHELL),
        OsString::from("-c"),
        OsString::from_vec(command.to_vec()),
    ]
}

/// Keeps the last [`cap`](Self::new) bytes written to it.
#[derive(Debug, Clone)]
pub struct BoundedTail {
    cap: usize,
    buf: VecDeque<u8>,
    dropped: bool,
}

impl BoundedTail {
    /// Empty buffer keeping at most `cap` bytes.
    pub fn new(cap: usize) -> Self {
        BoundedTail {
            cap,
            buf: VecDeque::new(),
            dropped: false,
        }
    }

    /// Append `chunk`, dropping the oldest bytes beyond the cap.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend(chunk);
        if self.buf.len() > self.cap {
            let extra = self.buf.len() - self.cap;
            self.buf.drain(..extra);
            self.dropped = true;
        }
    }

    /// The last `n` lines (lossy UTF-8, trailing newline ignored), with `...` first when older
    /// output was dropped or cut.
    pub fn last_lines(&self, n: usize) -> String {
        let bytes: Vec<u8> = self.buf.iter().copied().collect();
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<&str> = text.trim_end_matches('\n').lines().collect();
        // After a cut the first line is most likely partial: drop it (unless it is all we have).
        if self.dropped && lines.len() > 1 {
            lines.remove(0);
        }
        let skipped = lines.len().saturating_sub(n);
        let mut out = String::new();
        if skipped > 0 || (self.dropped && !lines.is_empty()) {
            out.push_str("...\n");
        }
        out.push_str(&lines[skipped..].join("\n"));
        out
    }

    /// Whether nothing (but whitespace) was captured.
    pub fn is_blank(&self) -> bool {
        self.buf.iter().all(u8::is_ascii_whitespace)
    }
}

/// Result of a background command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Exit status 0.
    Success {
        /// Tail of stdout.
        stdout: String,
    },
    /// Non-zero exit or killed by a signal.
    Failed {
        /// Exit code (`None` when killed by a signal).
        code: Option<i32>,
        /// Terminating signal, if any.
        signal: Option<i32>,
        /// Tail of stderr.
        stderr: String,
    },
    /// The command could not be started.
    SpawnFailed(String),
}

/// Run `command` through `sh -c` in `cwd` and wait for it (blocking; call from a worker).
/// stdin is closed; the child gets its own process group, so a Ctrl+C aimed at OFM's
/// terminal does not reach it.
pub fn run_blocking(command: &[u8], cwd: &Path) -> Outcome {
    let args = argv(command);
    let mut child = match Command::new(&args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Outcome::SpawnFailed(format!("{SHELL}: {e}")),
    };
    let out = child.stdout.take().map(spawn_reader);
    let err = child.stderr.take().map(spawn_reader);
    let status = child.wait();
    let stdout = out
        .and_then(|h| h.join().ok())
        .unwrap_or_else(|| BoundedTail::new(0));
    let stderr = err
        .and_then(|h| h.join().ok())
        .unwrap_or_else(|| BoundedTail::new(0));
    match status {
        Ok(s) if s.success() => Outcome::Success {
            stdout: if stdout.is_blank() {
                String::new()
            } else {
                stdout.last_lines(SHOWN_LINES)
            },
        },
        Ok(s) => Outcome::Failed {
            code: s.code(),
            signal: s.signal(),
            stderr: stderr.last_lines(SHOWN_LINES),
        },
        Err(e) => Outcome::SpawnFailed(format!("waiting for the command: {e}")),
    }
}

fn spawn_reader<R: Read + Send + 'static>(mut stream: R) -> std::thread::JoinHandle<BoundedTail> {
    std::thread::spawn(move || {
        let mut tail = BoundedTail::new(OUTPUT_CAP);
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => tail.push(&chunk[..n]),
            }
        }
        tail
    })
}

/// Start [`run_blocking`] on a dedicated thread; the receiver yields exactly one [`Outcome`]
/// (or closes without one if the thread died, which the caller reports).
pub fn spawn_background(command: Vec<u8>, cwd: PathBuf) -> async_channel::Receiver<Outcome> {
    let (tx, rx) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("ofm-usermenu".into())
        .spawn(move || {
            let outcome = run_blocking(&command, &cwd);
            let _ = tx.send_blocking(outcome);
        });
    if let Err(e) = spawned {
        tracing::error!("cannot start the user menu thread: {e}");
    }
    rx
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end_and_marks_cuts() {
        let mut t = BoundedTail::new(10);
        t.push(b"line1\nline2\n");
        assert_eq!(t.last_lines(5), "...\nline2");
        let mut t = BoundedTail::new(100);
        t.push(b"a\nb\nc\nd\n");
        assert_eq!(t.last_lines(2), "...\nc\nd");
        assert_eq!(t.last_lines(10), "a\nb\nc\nd");
        assert!(BoundedTail::new(4).is_blank());
    }

    #[test]
    fn argv_is_exact() {
        let a = argv(b"echo \xff 'x y'");
        assert_eq!(a[0], OsString::from("/bin/sh"));
        assert_eq!(a[1], OsString::from("-c"));
        assert_eq!(a[2].as_encoded_bytes(), b"echo \xff 'x y'");
    }

    #[test]
    fn quote_style_is_posix() {
        assert_eq!(quote_style(), QuoteStyle::Posix);
    }

    #[test]
    fn runs_in_the_directory_and_reports_exit_codes() {
        let dir = ofm_testkit::temp::TempDir::new().unwrap();
        let ok = run_blocking(b"pwd; echo done > marker", dir.path());
        let Outcome::Success { stdout } = ok else {
            panic!("{ok:?}");
        };
        let cwd = dir.path().canonicalize().unwrap();
        assert_eq!(stdout.trim(), cwd.to_string_lossy());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("marker")).unwrap(),
            "done\n"
        );
        let failed = run_blocking(b"echo oops >&2; exit 3", dir.path());
        assert_eq!(
            failed,
            Outcome::Failed {
                code: Some(3),
                signal: None,
                stderr: "oops".into()
            }
        );
        let killed = run_blocking(b"kill -9 $$", dir.path());
        assert!(matches!(
            killed,
            Outcome::Failed {
                code: None,
                signal: Some(9),
                ..
            }
        ));
        let missing = run_blocking(b"true", &dir.path().join("nope"));
        assert!(matches!(missing, Outcome::SpawnFailed(_)));
    }

    #[test]
    fn huge_output_is_bounded() {
        let dir = ofm_testkit::temp::TempDir::new().unwrap();
        let out = run_blocking(b"yes 0123456789 | head -c 3000000; exit 1", dir.path());
        let Outcome::Failed { stderr, .. } = out else {
            panic!("{out:?}");
        };
        assert!(stderr.is_empty());
        let out = run_blocking(b"yes 0123456789 | head -c 3000000 >&2; exit 1", dir.path());
        let Outcome::Failed { stderr, .. } = out else {
            panic!("{out:?}");
        };
        assert!(stderr.len() <= OUTPUT_CAP);
    }
}
