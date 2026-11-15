//! Shared helpers for the T3.4 torture suite (F17). Lives in `tests/torture_common/` so cargo does
//! not compile it as a test target of its own; each `torture_*.rs` binary pulls it in with
//! `mod torture_common;`.
//!
//! Everything here is deterministic (fixed seeds, injected clocks) and only ever writes inside
//! `TempDir`s. proptest is not a workspace dependency, so random trees use the small fixed-seed
//! [`Rng`] below instead (no new crates).

#![allow(dead_code)] // each test binary uses a different subset

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ofm_core::{CancelToken, Location};
use ofm_jobs::event::{Answer, ConflictAnswer, ErrorAction, Outcome, Progress, Question, Report};
use ofm_jobs::fs::{Fs, FsFile, FsStat, RealFs, TrashError};
use ofm_jobs::progress::Clock;
use ofm_jobs::spec::{CopyOpts, JobSpec};
use ofm_jobs::{JobCtx, JobSink, PauseGate, ScriptedSink, run_job, run_sync};

// -------------------------------------------------------------------------------------- rng

/// Deterministic splitmix64 generator (fixed seeds -> reproducible tests, no dependencies).
pub struct Rng(u64);

impl Rng {
    /// New generator; pick fixed seeds per test so failures reproduce.
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1))
    }

    /// Next raw value.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Value below `n` (modulo is fine for test data).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// Random bool.
    pub fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 0
    }

    /// Random byte.
    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 32) as u8
    }
}

// ------------------------------------------------------------------------------- fixtures

/// Marker every engine temp name contains (ADR-0004: `.<name>.ofm-part-<random>`).
pub const PART_MARKER: &str = ".ofm-part-";

/// Fresh source + destination temp dirs (`<tmp>/ofm-<tag>-src`, `...-dst`).
pub fn src_dst(tag: &str) -> (TempDirGuard, TempDirGuard) {
    (
        TempDirGuard::with_name(&format!("ofm-t34-{tag}-src")).unwrap(),
        TempDirGuard::with_name(&format!("ofm-t34-{tag}-dst")).unwrap(),
    )
}

/// `ofm_testkit::temp::TempDir` re-exported under a short name.
pub use ofm_testkit::temp::TempDir as TempDirGuard;

/// `Location::Local(path)`.
pub fn local(p: impl AsRef<Path>) -> Location {
    Location::Local(p.as_ref().to_path_buf())
}

/// Copy spec for local sources.
pub fn copy_spec(sources: &[PathBuf], dest_dir: &Path, opts: CopyOpts) -> JobSpec {
    JobSpec::Copy {
        sources: sources.iter().map(local).collect(),
        dest_dir: dest_dir.to_path_buf(),
        opts,
    }
}

/// Move spec for local sources.
pub fn move_spec(sources: &[PathBuf], dest_dir: &Path, opts: CopyOpts) -> JobSpec {
    JobSpec::Move {
        sources: sources.iter().map(local).collect(),
        dest_dir: dest_dir.to_path_buf(),
        opts,
    }
}

/// Names that stress path handling: bytes all the way, no shell, no `to_str()`.
/// (Temp-lookalike names are deliberately NOT here: `assert_no_part_files` scans for the marker.)
pub fn weird_names() -> Vec<OsString> {
    let long: String = "x".repeat(255);
    vec![
        "plain.txt".into(),
        "with space and.tab\tand;semicolon".into(),
        "new\nline.txt".into(),
        "back\\slash.txt".into(),
        "-leading-dash.txt".into(),
        "--double-dash".into(),
        "-".into(),
        "quote'and\"both".into(),
        "unicode-Ω-Ж-😀.txt".into(),
        "dot.dot.dot...txt".into(),
        "trailing space. ".into(),
        ofm_testkit::tree::raw_name(b"bad-utf8-\xfe\xfa.txt"),
        ofm_testkit::tree::raw_name(b"\xff-starts-with-invalid"),
        long.into(),
    ]
}

/// The single entry of `dir` (panics if there are more or none).
pub fn sole_entry(dir: &Path) -> PathBuf {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one entry in {}",
        dir.display()
    );
    entries.pop().unwrap()
}

const RANDOM_BASES: &[&str] = &[
    "file",
    "data",
    "notes",
    "a b",
    "dash-name",
    "осень",
    "Ω-file",
    "x",
    "tab\there",
];
const RANDOM_EXTS: &[&str] = &["", ".txt", ".md", ".tar.gz", ".bin"];

/// Deterministic random tree for property-style tests: files (random content/mode/mtime), dirs,
/// symlinks (relative to existing entries, dangling relative, dangling absolute), hardlinks to
/// earlier files of the same level, small sparse files. Never generates unreadable modes or
/// `.ofm-part-` lookalikes. `root_abs` is the future build root (for absolute symlink targets).
pub fn random_tree(rng: &mut Rng, root_abs: &Path, depth: u32) -> ofm_testkit::tree::TreeSpec {
    use ofm_testkit::tree::{DirSpec, FileSpec, TreeSpec};
    let mut tree = TreeSpec::new();
    let mut used: Vec<OsString> = Vec::new();
    let mut last_file: Option<OsString> = None;
    let count = 2 + rng.below(4) as usize;
    for i in 0..count {
        let mut name = format!(
            "{}{}",
            RANDOM_BASES[rng.below(RANDOM_BASES.len() as u64) as usize],
            RANDOM_EXTS[rng.below(RANDOM_EXTS.len() as u64) as usize]
        );
        if rng.below(4) == 0 {
            name = format!("{name}-{}", rng.below(1000));
        }
        if used.iter().any(|u| u.to_string_lossy() == name) {
            name = format!("{name}.{i}");
        }
        let name: OsString = if rng.below(8) == 0 {
            ofm_testkit::tree::raw_name(name.to_string().as_bytes())
        } else {
            name.into()
        };
        if used.contains(&name) {
            continue;
        }
        let kind = rng.below(11);
        if kind <= 5 {
            // regular file
            let len = rng.below(8192) as usize;
            let content: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            let mode = [0o644, 0o600, 0o755, 0o444, 0o400][rng.below(5) as usize];
            let mtime =
                ofm_testkit::tree::DEFAULT_MTIME_NS + rng.below(1_000_000_000_000_000) as i128;
            tree = tree.file(
                name.clone(),
                FileSpec::content(&content).mode(mode).mtime_ns(mtime),
            );
            last_file = Some(name.clone());
        } else if kind <= 6 && depth < 3 {
            let mode = [0o755, 0o700, 0o555, 0o750][rng.below(4) as usize];
            let children = random_tree(rng, root_abs, depth + 1);
            tree = tree.dir(name.clone(), DirSpec::new(children).mode(mode));
        } else if kind <= 8 {
            // symlink: relative to an existing sibling, dangling relative, or dangling absolute
            let target = match rng.below(3) {
                0 => used
                    .last()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("nothing-here")),
                1 => PathBuf::from(format!("dangling-{}", rng.below(100))),
                _ => root_abs.join(format!("dangling-abs-{}", rng.below(100))),
            };
            tree = tree.symlink(name.clone(), target);
        } else if kind == 9 && last_file.is_some() {
            let target = last_file.clone().unwrap();
            tree = tree.hardlink(name.clone(), target);
        } else {
            // sparse file with zero or one data range
            let size = 1 + rng.below(256 * 1024);
            let file = if rng.below(2) == 0 {
                FileSpec::sparse(size)
            } else {
                let off = rng.below(size);
                let data: Vec<u8> = (0..64).map(|_| rng.byte()).collect();
                FileSpec::sparse_ranges(size, &[(off, &data)])
            };
            tree = tree.file(name.clone(), file.mode(0o644));
            last_file = Some(name.clone());
        }
        used.push(name);
    }
    tree
}

/// Write a small file with exact mode and mtime (fixture prep; NOT through the engine).
pub fn write_file(path: &Path, content: &[u8], mode: u32, mtime_ns: i128) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
    RealFs.set_times(path, mtime_ns, mtime_ns, false).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

// ------------------------------------------------------------------------------- running

/// Run `spec` to completion with a refusing sink (asserts nothing was asked unless you check).
pub fn run(spec: &JobSpec, fs: Arc<dyn Fs>, sink: impl JobSink + 'static) -> Outcome {
    run_sync(spec, fs, sink)
}

/// Run `spec` with a fixed policy for every question (apply_to_all = true).
pub fn run_policy(
    spec: &JobSpec,
    fs: Arc<dyn Fs>,
    conflict: ConflictAnswer,
    error: ErrorAction,
    permanent: bool,
) -> (Outcome, Arc<Mutex<ofm_jobs::Recording>>) {
    let (sink, rec) = ScriptedSink::policy(conflict, error, permanent);
    (run_sync(spec, fs, sink), rec)
}

/// Answers conflicts per source *file name* (order-independent), errors with `error`.
/// A conflict for an unlisted name gets `Skip` with `apply_to_all = false`.
pub fn answers_by_name(
    map: &[(OsString, ConflictAnswer, bool)],
    error: ErrorAction,
) -> (ScriptedSink, Arc<Mutex<ofm_jobs::Recording>>) {
    let map = map
        .iter()
        .map(|(n, a, all)| (n.clone(), *a, *all))
        .collect::<Vec<_>>();
    ScriptedSink::new(move |q| match q {
        Question::Conflict { source, .. } => {
            let name = source.path.file_name().unwrap_or_default();
            match map.iter().find(|(n, _, _)| n == name) {
                Some((_, action, all)) => Some(Answer::Conflict {
                    action: *action,
                    apply_to_all: *all,
                }),
                None => Some(Answer::Conflict {
                    action: ConflictAnswer::Skip,
                    apply_to_all: false,
                }),
            }
        }
        Question::Error { .. } => Some(Answer::Error {
            action: error,
            apply_to_all: false,
        }),
        Question::TrashUnavailable { .. } => Some(Answer::Confirm {
            permanent: true,
            apply_to_all: false,
        }),
    })
}

// --------------------------------------------------------------------- cancel machinery

/// Clock that advances 150 ms on every read: every `JobCtx` progress pump emits a snapshot
/// (10 Hz throttle sees a new interval each time), so a sink observes the byte counters
/// immediately after each tracker update. Deterministic, wall-clock independent.
pub struct TickClock(AtomicU64);

impl TickClock {
    /// New clock at t = 0.
    pub fn new() -> Self {
        TickClock(AtomicU64::new(0))
    }
}

impl Clock for TickClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.fetch_add(150_000_000, Ordering::Relaxed))
    }
}

/// Sink that cancels the job's token as soon as a progress snapshot reports
/// `bytes_done >= at`, and refuses every question (recording them).
#[derive(Clone)]
pub struct CancelSink {
    inner: Arc<CancelSinkInner>,
}

struct CancelSinkInner {
    cancel: CancelToken,
    at: u64,
    fired: AtomicBool,
    questions: Mutex<Vec<String>>,
}

impl CancelSink {
    /// Cancel the job once `bytes_done` reaches `at`.
    pub fn new(cancel: CancelToken, at: u64) -> Self {
        CancelSink {
            inner: Arc::new(CancelSinkInner {
                cancel,
                at,
                fired: AtomicBool::new(false),
                questions: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Whether the cancellation fired.
    pub fn fired(&self) -> bool {
        self.inner.fired.load(Ordering::Relaxed)
    }

    /// Questions seen (rendered) before the job stopped.
    pub fn questions(&self) -> Vec<String> {
        self.inner
            .questions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl JobSink for CancelSink {
    fn progress(&mut self, p: &Progress) {
        if !self.inner.fired.load(Ordering::Relaxed) && p.bytes_done >= self.inner.at {
            self.inner.fired.store(true, Ordering::Relaxed);
            self.inner.cancel.cancel();
        }
    }

    fn ask(&mut self, question: Question) -> Option<Answer> {
        self.inner
            .questions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{question:?}"));
        None
    }
}

/// Run `spec` so that cancellation is requested once `bytes_done >= at` (see [`CancelSink`]).
pub fn run_cancel_at(spec: &JobSpec, fs: Arc<dyn Fs>, at: u64) -> (Outcome, CancelSink) {
    let cancel = CancelToken::new();
    let sink = CancelSink::new(cancel.clone(), at);
    let mut ctx = JobCtx::new(
        fs,
        cancel,
        PauseGate::new(),
        Box::new(sink.clone()),
        Arc::new(TickClock::new()),
    );
    (run_job(spec, &mut ctx), sink)
}

/// Run with an already-cancelled token: the job must stop before doing anything.
pub fn run_pre_cancelled(spec: &JobSpec, fs: Arc<dyn Fs>) -> Outcome {
    let cancel = CancelToken::new();
    cancel.cancel();
    let (sink, _) = ScriptedSink::refusing();
    let mut ctx = JobCtx::new(
        fs,
        cancel,
        PauseGate::new(),
        Box::new(sink),
        Arc::new(TickClock::new()),
    );
    run_job(spec, &mut ctx)
}

// ---------------------------------------------------------------------------- assertions

/// Every entry under `dir` (recursively, never following symlinks) whose name contains the
/// engine temp marker. A leftover means a cleanup path missed an error/cancel.
pub fn part_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_parts(dir, &mut found, 0);
    found
}

fn collect_parts(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    if depth > 300 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name
            .as_bytes()
            .windows(PART_MARKER.len())
            .any(|w| w == PART_MARKER.as_bytes())
        {
            out.push(path.clone());
        }
        if entry
            .file_type()
            .map(|t| t.is_dir() && !t.is_symlink())
            .unwrap_or(false)
        {
            collect_parts(&path, out, depth + 1);
        }
    }
}

/// Assert no `.ofm-part-*` entry survives under `dir`.
pub fn assert_no_part_files(dir: &Path) {
    let parts = part_files_in(dir);
    assert!(
        parts.is_empty(),
        "engine temp files survived under {}: {parts:?}",
        dir.display()
    );
}

/// Data-safety core assertion: `path` is either absent or byte-exact — never truncated or
/// half-written under its final name.
pub fn assert_absent_or_exact(path: &Path, content: &[u8], what: &str) {
    match std::fs::read(path) {
        Ok(bytes) => assert!(
            bytes == content,
            "{what}: {} exists but content differs (len {} != {})",
            path.display(),
            bytes.len(),
            content.len()
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => panic!("{what}: {} unreadable: {e}", path.display()),
    }
}

/// Full equality: testkit content/mode/mtime/xattr compare PLUS directory mode/mtime
/// (which `compare_trees` does not check for directories themselves). The roots themselves are
/// only containers (temp/dest dirs nobody assigns a fixed mtime to); their CHILDREN are compared.
pub fn assert_full_tree_equal(a: &Path, b: &Path) {
    ofm_testkit::compare::assert_trees_equal(a, b, ofm_testkit::compare::Compare::all());
    let Ok(entries) = std::fs::read_dir(a) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() && !ft.is_symlink() {
            assert_dir_meta_recursive(&entry.path(), &b.join(entry.file_name()), 0);
        }
    }
}

fn assert_dir_meta_recursive(a: &Path, b: &Path, depth: u32) {
    if depth > 300 {
        return;
    }
    let (ma, mb) = (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b));
    let (Ok(ma), Ok(mb)) = (ma, mb) else {
        return; // a missing counterpart is already reported by compare_trees
    };
    if !ma.is_dir() || ma.is_symlink() {
        return;
    }
    assert_eq!(
        ma.mode() & 0o7777,
        mb.mode() & 0o7777,
        "dir mode differs for {}",
        a.display()
    );
    assert_eq!(
        mtime_ns(&ma),
        mtime_ns(&mb),
        "dir mtime differs for {}",
        a.display()
    );
    let Ok(entries) = std::fs::read_dir(a) else {
        return;
    };
    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() && !ft.is_symlink() {
            assert_dir_meta_recursive(&entry.path(), &b.join(entry.file_name()), depth + 1);
        }
    }
}

/// mtime of a `symlink_metadata` result, ns since epoch.
pub fn mtime_ns(md: &std::fs::Metadata) -> i128 {
    md.mtime() as i128 * 1_000_000_000 + md.mtime_nsec() as i128
}

/// Blocks allocated (`st_blocks`) of `path` (lstat).
pub fn blocks_of(path: &Path) -> u64 {
    std::fs::symlink_metadata(path)
        .map(|m| m.blocks())
        .unwrap_or(0)
}

/// Device id (`st_dev`) of `path`.
pub fn st_dev_of(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.dev()).unwrap_or(0)
}

/// Whether the current filesystem supports `user.*` xattrs (skips xattr asserts if not).
pub fn xattrs_supported(dir: &Path) -> bool {
    let probe = dir.join("ofm-xattr-probe");
    std::fs::write(&probe, b"x").is_ok()
        && RealFs
            .set_xattr(&probe, OsStr::from_bytes(b"user.ofm.probe"), b"v", false)
            .is_ok()
}

/// Report of an outcome, with a context message.
pub fn report_of(outcome: &Outcome, what: &str) -> Report {
    outcome
        .report()
        .cloned()
        .unwrap_or_else(|| panic!("{what}: outcome carries no report: {outcome:?}"))
}

/// `true` when the suite runs as root (permission-denied faults would not fire then).
pub fn running_as_root() -> bool {
    RealFs.is_root()
}

/// Two temp dirs on two different real filesystems, or `None` (caller skips with a log line).
pub fn two_filesystems() -> Option<ofm_testkit::crossdev::FilesystemPair> {
    ofm_testkit::crossdev::two_filesystems()
}

// ---------------------------------------------------------- trash-unavailable Fs wrapper

/// [`Fs`] wrapper whose `trash` always fails with `TrashError::Unavailable` — the hermetic way to
/// drive the `Question::TrashUnavailable` / `Answer::Confirm` protocol (a `FaultPlan` can only
/// inject `io::Error`s, which map to `TrashError::Io`, never `Unavailable`).
pub struct TrashUnavailableFs {
    inner: Arc<dyn Fs>,
}

impl TrashUnavailableFs {
    /// Wrap the real filesystem.
    pub fn new() -> Self {
        TrashUnavailableFs {
            inner: Arc::new(RealFs),
        }
    }
}

impl Fs for TrashUnavailableFs {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.inner.open_read(path)
    }
    fn open_read_follow(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.inner.open_read_follow(path)
    }
    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>> {
        self.inner.create_excl(path, mode)
    }
    fn stat(&self, path: &Path) -> io::Result<FsStat> {
        self.inner.stat(path)
    }
    fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        self.inner.lstat(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        self.inner.read_dir(path)
    }
    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.inner.mkdir(path, mode)
    }
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename_noreplace(from, to)
    }
    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename_replace(from, to)
    }
    fn unlink(&self, path: &Path) -> io::Result<()> {
        self.inner.unlink(path)
    }
    fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.inner.rmdir(path)
    }
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        self.inner.symlink(target, link)
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.hard_link(from, to)
    }
    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.inner.read_link(path)
    }
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.inner.set_mode(path, mode)
    }
    fn set_times(
        &self,
        path: &Path,
        atime_ns: i128,
        mtime_ns: i128,
        follow: bool,
    ) -> io::Result<()> {
        self.inner.set_times(path, atime_ns, mtime_ns, follow)
    }
    fn set_owner(
        &self,
        path: &Path,
        uid: Option<u32>,
        gid: Option<u32>,
        follow: bool,
    ) -> io::Result<()> {
        self.inner.set_owner(path, uid, gid, follow)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
    }
    fn list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>> {
        self.inner.list_xattrs(path, follow)
    }
    fn get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>> {
        self.inner.get_xattr(path, name, follow)
    }
    fn set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()> {
        self.inner.set_xattr(path, name, value, follow)
    }
    fn trash(&self, _path: &Path) -> Result<(), TrashError> {
        Err(TrashError::Unavailable(
            "torture: trash unavailable by construction".into(),
        ))
    }
    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
}
