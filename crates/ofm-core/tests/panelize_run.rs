//! Integration tests for `ofm_core::panelize::run_panelize` against a real shell (T7.2, F25).
//! All tests are deterministic and skip nothing: `/bin/sh` is part of the target platform.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ofm_core::cancel::CancelToken;
use ofm_core::panelize::{
    Delimiter, MAX_PATHS, PanelizeError, PanelizeEvent, PanelizeSpec, run_panelize,
    run_panelize_timed,
};

fn spec(cwd: &Path, command: &str) -> PanelizeSpec {
    PanelizeSpec {
        cwd: cwd.to_path_buf(),
        command: command.into(),
        shell: Some("/bin/sh".into()),
        delimiter: Delimiter::Newline,
    }
}

struct Events(Vec<PanelizeEvent>);

impl Events {
    fn collect(spec: &PanelizeSpec, cancel: &CancelToken) -> Events {
        let mut events = Vec::new();
        run_panelize(spec, cancel, &mut |ev| events.push(ev)).unwrap();
        Events(events)
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.0
            .iter()
            .filter_map(|ev| match ev {
                PanelizeEvent::Paths(p) => Some(p.iter().cloned()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn warnings(&self) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|ev| match ev {
                PanelizeEvent::Warning(w) => Some(w.clone()),
                _ => None,
            })
            .collect()
    }

    fn done(&self) -> &ofm_core::panelize::PanelizeSummary {
        self.0
            .iter()
            .find_map(|ev| match ev {
                PanelizeEvent::Done(s) => Some(s),
                _ => None,
            })
            .expect("exactly one Done event")
    }
}

#[test]
fn relative_absolute_spaced_and_dashed_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    // Real files, including a leading-dash name and one with spaces.
    for name in ["plain", "with space", "-leading-dash", "./dot"] {
        let p = cwd.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
    }
    let abs = cwd.join("abs file");
    std::fs::write(&abs, b"x").unwrap();

    let ev = Events::collect(
        &spec(
            cwd,
            "printf '%s\\n' plain 'with space' ./-leading-dash ./dot; printf '%s\\n' \"$PWD/abs file\"",
        ),
        &CancelToken::new(),
    );
    assert_eq!(
        ev.paths(),
        [
            cwd.join("plain"),
            cwd.join("with space"),
            cwd.join("-leading-dash"),
            cwd.join("dot"),
            abs,
        ]
    );
    assert_eq!(ev.done().exit_code, Some(0));
    assert_eq!(ev.done().total_paths, 5);
    assert!(ev.warnings().is_empty());
    // Exactly one terminal Done, last event.
    assert!(matches!(ev.0.last(), Some(PanelizeEvent::Done(_))));
}

#[test]
fn trailing_partial_line_and_crlf_are_handled() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    std::fs::write(cwd.join("a"), b"").unwrap();
    std::fs::write(cwd.join("b"), b"").unwrap();
    let ev = Events::collect(&spec(cwd, "printf 'a\\r\\nb'"), &CancelToken::new());
    assert_eq!(ev.paths(), [cwd.join("a"), cwd.join("b")]);
}

#[test]
fn nul_mode_keeps_newlines_and_non_utf8_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    use std::os::unix::ffi::OsStrExt;
    let weird = cwd.join(std::ffi::OsStr::from_bytes(b"w\xffth\nnewline"));
    std::fs::write(&weird, b"x").unwrap();
    let mut s = spec(cwd, "printf 'w\\377th\\nnewline\\0plain\\0'");
    s.delimiter = Delimiter::Nul;
    let ev = Events::collect(&s, &CancelToken::new());
    assert_eq!(ev.paths(), [weird, cwd.join("plain")]);
    assert!(ev.warnings().is_empty());
}

#[test]
fn nonzero_exit_keeps_partial_results_and_stderr_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    std::fs::write(cwd.join("kept"), b"").unwrap();
    let ev = Events::collect(
        &spec(
            cwd,
            "printf 'kept\\nvapor\\n'; echo 'some warning text' >&2; exit 3",
        ),
        &CancelToken::new(),
    );
    // Partial results before the non-zero exit are kept (paths are not checked for existence
    // here — that is the list provider's job).
    assert_eq!(ev.paths(), [cwd.join("kept"), cwd.join("vapor")]);
    let done = ev.done();
    assert_eq!(done.exit_code, Some(3));
    assert!(done.stderr_tail.contains("some warning text"));
    assert!(!done.cancelled);
}

#[test]
fn spawn_failure_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let mut bad = spec(tmp.path(), "true");
    bad.shell = Some("/no/such/shell".into());
    let err = run_panelize(&bad, &CancelToken::new(), &mut |_| {});
    assert!(matches!(err, Err(PanelizeError::Spawn { .. })));
}

#[test]
fn over_long_items_warn_and_are_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    // 6000 bytes without a newline: over the 4096 cap; followed by a healthy line.
    let ev = Events::collect(
        &spec(
            cwd,
            "head -c 6000 /dev/zero | tr '\\0' x; printf '\\nhealthy\\n'",
        ),
        &CancelToken::new(),
    );
    assert_eq!(ev.paths(), [cwd.join("healthy")]);
    assert_eq!(ev.warnings().len(), 1);
    assert!(!ev.done().cancelled);
}

#[test]
fn huge_output_hits_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let ev = Events::collect(&spec(tmp.path(), "seq 1 3000000"), &CancelToken::new());
    let done = ev.done();
    assert!(done.truncated, "summary must mark truncation");
    assert_eq!(done.total_paths, MAX_PATHS);
    assert_eq!(ev.paths().len(), MAX_PATHS);
}

#[test]
fn cancel_kills_sleeping_command_promptly_and_leaves_no_orphans() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let marker = cwd.join("child.pid");
    // The command backgrounds a grandchild and waits: killing only the shell would orphan it.
    let s = spec(
        cwd,
        &format!("sleep 60 & echo $! > {}; wait", marker.display()),
    );
    let cancel = Arc::new(CancelToken::new());
    let worker_cancel = cancel.clone();
    let handle = std::thread::spawn(move || {
        let mut events = Vec::new();
        run_panelize(&s, &worker_cancel, &mut |ev| events.push(ev)).unwrap();
        events
    });
    std::thread::sleep(Duration::from_millis(150));
    let t0 = Instant::now();
    cancel.cancel();
    let events = handle.join().unwrap();
    assert!(
        t0.elapsed() < Duration::from_millis(500),
        "cancel must return in < 500 ms, took {:?}",
        t0.elapsed()
    );
    let done = events
        .iter()
        .find_map(|ev| match ev {
            PanelizeEvent::Done(s) => Some(s.clone()),
            _ => None,
        })
        .expect("Done event");
    assert!(done.cancelled);
    assert!(matches!(events.last(), Some(PanelizeEvent::Done(_))));

    // No orphan: the backgrounded `sleep 60` must be dead (poll: it can be a zombie for a moment).
    let child_pid: u32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let proc_dir = PathBuf::from(format!("/proc/{child_pid}"));
    let reap_deadline = Instant::now() + Duration::from_secs(2);
    while proc_dir.exists() && Instant::now() < reap_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !proc_dir.exists(),
        "grandchild process {child_pid} survived the group kill"
    );
}

#[test]
fn timeout_kills_the_command() {
    let tmp = tempfile::tempdir().unwrap();
    let s = spec(tmp.path(), "sleep 60");
    let mut events = Vec::new();
    let started = Instant::now();
    run_panelize_timed(
        &s,
        &CancelToken::new(),
        Some(Duration::from_millis(200)),
        &mut |ev| events.push(ev),
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let done = events
        .iter()
        .find_map(|ev| match ev {
            PanelizeEvent::Done(s) => Some(s.clone()),
            _ => None,
        })
        .expect("Done event");
    assert!(done.cancelled, "timeout is reported as cancelled");
}

#[test]
fn command_gets_one_argv_element_and_stdin_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    // A command with spaces/semicolons must run as-is (one argv element after -c), and stdin
    // must be closed (cat returns at once).
    let ev = Events::collect(
        &spec(
            cwd,
            "for f in a b; do printf '%s\\n' \"$f\"; done; cat >/dev/null 2>&1 || true",
        ),
        &CancelToken::new(),
    );
    assert_eq!(ev.paths(), [cwd.join("a"), cwd.join("b")]);
    assert_eq!(ev.done().exit_code, Some(0));
}
