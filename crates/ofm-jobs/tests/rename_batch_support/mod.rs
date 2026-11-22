//! Shared helpers of the T8.3b batch rename tests. Every test works in `<tmp>/work` and keeps the
//! undo log in `<tmp>/state/undo.toml`, so tree comparisons never see the log.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ofm_core::CancelToken;
use ofm_core::batch_rename::{Plan, PlanOptions, RenameOp, plan_with_options};
use ofm_jobs::progress::SystemClock;
use ofm_jobs::{
    FileId, Fs, JobCtx, JobSpec, Outcome, PauseGate, RenameBatchSpec, Report, RollbackSpec,
    ScriptedSink, run_job, run_sync,
};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{FileSpec, TreeSpec};

pub use ofm_jobs::Recording;

pub type Rec = Arc<std::sync::Mutex<Recording>>;

/// Name -> content of the files directly in `dir` (content of a fixture file is its original name).
pub type Listing = BTreeMap<OsString, Vec<u8>>;

pub struct Env {
    pub root: TempDir,
    pub work: PathBuf,
    pub log: PathBuf,
}

/// Fixture spec: regular files whose content is their name, fixed mtime/mode.
pub fn fixture_spec(files: &[&str]) -> TreeSpec {
    let mut spec = TreeSpec::new();
    for f in files {
        spec = spec.file(*f, FileSpec::content(f.as_bytes()));
    }
    spec
}

pub fn env(files: &[&str]) -> Env {
    let root = TempDir::new().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    fixture_spec(files).build(&work).unwrap();
    let log = root.path().join("state/undo.toml");
    Env { root, work, log }
}

/// A pristine copy of the fixture for `assert_trees_equal`.
pub fn pristine(files: &[&str]) -> TempDir {
    fixture_spec(files).build_temp().unwrap()
}

pub fn names(dir: &Path) -> HashSet<OsString> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect()
}

pub fn listing(dir: &Path) -> Listing {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let content = std::fs::read(e.path()).unwrap_or_default();
            (e.file_name(), content)
        })
        .collect()
}

pub fn expect(pairs: &[(&str, &str)]) -> Listing {
    pairs
        .iter()
        .map(|(n, c)| (OsString::from(n), c.as_bytes().to_vec()))
        .collect()
}

pub fn ops(pairs: &[(&str, &str)]) -> Vec<RenameOp> {
    pairs
        .iter()
        .map(|(a, b)| RenameOp {
            from: OsString::from(a),
            to: OsString::from(b),
        })
        .collect()
}

pub fn plan_of(work: &Path, pairs: &[(&str, &str)]) -> Plan {
    plan_ci(work, pairs, false)
}

pub fn plan_ci(work: &Path, pairs: &[(&str, &str)], ci: bool) -> Plan {
    plan_with_options(
        &ops(pairs),
        &names(work),
        &PlanOptions {
            assume_case_insensitive: ci,
        },
    )
    .unwrap()
}

pub fn batch(env: &Env, plan: Plan) -> RenameBatchSpec {
    RenameBatchSpec {
        dir: env.work.clone(),
        plan,
        undo_log: env.log.clone(),
        identities: Vec::new(),
    }
}

pub fn rollback_spec(env: &Env) -> RollbackSpec {
    RollbackSpec {
        log: ofm_jobs::load_log(&env.log).unwrap(),
        log_path: env.log.clone(),
    }
}

pub fn run_on(spec: JobSpec, fs: Arc<dyn Fs>) -> (Outcome, Rec) {
    let (sink, rec) = ScriptedSink::refusing();
    (run_sync(&spec, fs, sink), rec)
}

/// Runs a spec; asserts nobody was asked a question.
pub fn run_quiet(spec: JobSpec, fs: Arc<dyn Fs>) -> Outcome {
    let (out, rec) = run_on(spec, fs);
    assert!(
        rec.lock().unwrap().questions.is_empty(),
        "no questions expected"
    );
    out
}

pub fn real() -> Arc<dyn Fs> {
    Arc::new(ofm_jobs::RealFs)
}

/// Runs `spec` with a cancel token that a hook can trigger.
pub fn run_cancellable(spec: JobSpec, fs: Arc<dyn Fs>, cancel: CancelToken) -> (Outcome, Rec) {
    let (sink, rec) = ScriptedSink::refusing();
    let mut ctx = JobCtx::new(
        fs,
        cancel,
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    (run_job(&spec, &mut ctx), rec)
}

/// An `lstat` counter hook: calls `f()` on the n-th `lstat` (1-based).
pub fn on_nth_lstat(
    n: u64,
    f: impl Fn() + Send + Sync + 'static,
) -> impl Fn(&'static str) + Send + Sync {
    let count = AtomicU64::new(0);
    move |op| {
        if op == "lstat" && count.fetch_add(1, Ordering::SeqCst) + 1 == n {
            f();
        }
    }
}

pub fn report(out: &Outcome) -> &Report {
    out.report().expect("outcome has a report")
}

pub fn failed(out: &Outcome) -> (&str, Option<&Path>) {
    match out {
        Outcome::Failed { error, path, .. } => (error, path.as_deref()),
        other => panic!("expected Failed, got {other:?}"),
    }
}

pub fn log_steps(env: &Env) -> usize {
    ofm_jobs::load_log(&env.log).unwrap().steps.len()
}

pub fn identities(env: &Env, plan: &Plan) -> Vec<(OsString, FileId)> {
    ofm_jobs::snapshot_identities(&ofm_jobs::RealFs, &env.work, plan).unwrap()
}

pub fn is_root() -> bool {
    rustix::process::getuid().is_root()
}
