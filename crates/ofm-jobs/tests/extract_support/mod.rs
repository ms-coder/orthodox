//! Shared helpers of the T6.2 extract tests: an in-memory `ArchiveReader` (so the job is tested
//! independently of the real format backends), a hostile-reader hook, run helpers and file system
//! assertions. Only ever writes inside temp dirs.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofm_core::CancelToken;
use ofm_jobs::event::{Answer, ConflictAnswer, ErrorAction, Question, Report};
use ofm_jobs::extract::extract_from_reader;
use ofm_jobs::progress::SystemClock;
use ofm_jobs::{ExtractOpts, Fs, JobCtx, JobError, PauseGate, RealFs, Recording, ScriptedSink};
use ofm_vfs::archive::{
    ArchiveEntry, ArchiveEntryKind, ArchiveError, ArchiveFormat, ArchiveIndex, ArchiveLimits,
    ArchiveReader, EntryId, EntrySafety, IndexBuilder, RawEntry, VisitFn,
};

pub const PART_MARKER: &str = ".ofm-part-";

/// 2001-09-09T01:46:40Z.
pub const MTIME: i128 = ofm_testkit::tree::DEFAULT_MTIME_NS;

pub type Rec = Arc<Mutex<Recording>>;

// ------------------------------------------------------------------------------ temp dir

/// Temp dir that makes everything writable again before it is removed (read-only directory
/// entries must not leak into the temp area).
pub struct Tmp(ofm_testkit::temp::TempDir);

impl Tmp {
    pub fn new(tag: &str) -> Tmp {
        Tmp(ofm_testkit::temp::TempDir::with_name(&format!("ofm-t62-{tag}-")).unwrap())
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// `<tmp>/dst`, created.
    pub fn dst(&self) -> PathBuf {
        let d = self.path().join("dst");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A tiny real file standing for the archive (identity for `SameFile`, size for ratios).
    pub fn archive_file(&self, name: &str, size: usize) -> PathBuf {
        let p = self.path().join(name);
        std::fs::write(&p, vec![b'a'; size]).unwrap();
        p
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        fn unlock(dir: &Path) {
            let Ok(md) = std::fs::symlink_metadata(dir) else {
                return;
            };
            if !md.is_dir() {
                return;
            }
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    unlock(&e.path());
                }
            }
        }
        unlock(self.0.path());
    }
}

// ----------------------------------------------------------------------------- the archive

/// What an entry's stream does.
#[derive(Clone)]
pub enum Data {
    /// These bytes.
    Bytes(Vec<u8>),
    /// `n` zero bytes, produced lazily (bombs).
    Zeros(u64),
    /// `good` bytes, then a corrupt-archive read error.
    Corrupt(Vec<u8>),
    /// `good` bytes, then a decoder panic.
    PanicAfter(Vec<u8>),
    /// `good` bytes, then the given error, as the real backends report it (wrapped in `io::Error`).
    Fatal(Vec<u8>, FatalKind),
}

#[derive(Clone, Copy)]
pub enum FatalKind {
    NeedsPassword,
    WrongPassword,
    Limit,
}

impl FatalKind {
    fn error(self) -> ArchiveError {
        let archive = PathBuf::from("/mem.zip");
        match self {
            FatalKind::NeedsPassword => ArchiveError::NeedsPassword(archive),
            FatalKind::WrongPassword => ArchiveError::WrongPassword(archive),
            FatalKind::Limit => ArchiveError::LimitExceeded {
                archive,
                kind: ofm_vfs::archive::LimitKind::ExceedsDeclaredSize,
            },
        }
    }
}

#[derive(Clone)]
pub struct MemEntry {
    pub raw: RawEntry,
    pub data: Data,
}

/// Builder of an in-memory archive.
pub struct MemArchive {
    pub archive: PathBuf,
    pub entries: Vec<MemEntry>,
    pub limits: ArchiveLimits,
}

impl MemArchive {
    pub fn new(archive: &Path) -> MemArchive {
        MemArchive {
            archive: archive.to_path_buf(),
            entries: Vec::new(),
            limits: ArchiveLimits::default(),
        }
    }

    fn push(&mut self, mut raw: RawEntry, data: Data) -> &mut Self {
        raw.mtime_ns = Some(MTIME);
        self.entries.push(MemEntry { raw, data });
        self
    }

    pub fn file(&mut self, path: impl AsRef<std::ffi::OsStr>, bytes: &[u8]) -> &mut Self {
        let mut raw = RawEntry::new(PathBuf::from(path.as_ref()), ArchiveEntryKind::File);
        raw.size = bytes.len() as u64;
        raw.mode = Some(0o644);
        self.push(raw, Data::Bytes(bytes.to_vec()))
    }

    /// A file whose header declares `declared` bytes but whose stream is `data`.
    pub fn file_with(&mut self, path: &str, declared: u64, data: Data) -> &mut Self {
        let mut raw = RawEntry::new(path, ArchiveEntryKind::File);
        raw.size = declared;
        raw.mode = Some(0o644);
        self.push(raw, data)
    }

    pub fn dir(&mut self, path: &str) -> &mut Self {
        let mut raw = RawEntry::new(path, ArchiveEntryKind::Dir);
        raw.mode = Some(0o755);
        self.push(raw, Data::Bytes(Vec::new()))
    }

    pub fn symlink(&mut self, path: &str, target: &str) -> &mut Self {
        let mut raw = RawEntry::new(path, ArchiveEntryKind::Symlink);
        raw.link_target = Some(target.into());
        self.push(raw, Data::Bytes(Vec::new()))
    }

    pub fn hardlink(&mut self, path: &str, target: &str) -> &mut Self {
        let mut raw = RawEntry::new(path, ArchiveEntryKind::Hardlink);
        raw.link_target = Some(target.into());
        // Contract: a hard link's stream is its target's data (tar stores size 0 for the link).
        let data = self
            .entries
            .iter()
            .rev()
            .find(|e| e.raw.path == Path::new(target))
            .map_or(Data::Bytes(Vec::new()), |e| e.data.clone());
        self.push(raw, data)
    }

    pub fn other(&mut self, path: &str) -> &mut Self {
        let raw = RawEntry::new(path, ArchiveEntryKind::Other);
        self.push(raw, Data::Bytes(Vec::new()))
    }

    /// Modify the last pushed entry.
    pub fn with(&mut self, f: impl FnOnce(&mut RawEntry)) -> &mut Self {
        if let Some(e) = self.entries.last_mut() {
            f(&mut e.raw);
        }
        self
    }

    pub fn mode(&mut self, mode: u32) -> &mut Self {
        self.with(|r| r.mode = Some(mode))
    }

    pub fn mtime(&mut self, ns: i128) -> &mut Self {
        self.with(|r| r.mtime_ns = Some(ns))
    }

    pub fn packed(&mut self, n: u64) -> &mut Self {
        self.with(|r| r.packed_size = Some(n))
    }

    pub fn reader(&self) -> MemReader {
        let mut b = IndexBuilder::new(&self.archive, ArchiveFormat::Zip, self.limits);
        let mut datas = HashMap::new();
        for e in &self.entries {
            if let Some(id) = b.push(e.raw.clone()).unwrap() {
                datas.insert(id, e.data.clone());
            }
        }
        MemReader {
            index: Arc::new(b.finish().unwrap()),
            datas,
            tamper: None,
            chunk: 4096,
            on_read: None,
            visit_error: None,
        }
    }
}

type Tamper = Box<dyn Fn(&ArchiveEntry) -> Vec<ArchiveEntry> + Send>;
type OnRead = Arc<dyn Fn(u64) + Send + Sync>;

/// The in-memory reader.
pub struct MemReader {
    pub index: Arc<ArchiveIndex>,
    datas: HashMap<EntryId, Data>,
    /// Replace what `visit` hands out for an entry (a hostile reader/index mismatch).
    pub tamper: Option<Tamper>,
    /// Bytes per `read` call.
    pub chunk: usize,
    /// Called with the cumulative number of data bytes delivered, before every read.
    pub on_read: Option<OnRead>,
    /// `visit` fails with this before delivering anything (password prompt, damaged archive).
    pub visit_error: Option<Box<dyn Fn() -> ArchiveError + Send>>,
}

struct Stream {
    data: Data,
    pos: u64,
    chunk: usize,
    cancel: CancelToken,
    base: Arc<Mutex<u64>>,
    on_read: Option<OnRead>,
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other(ArchiveError::Cancelled));
        }
        if let Some(f) = &self.on_read {
            f(*self.base.lock().unwrap());
        }
        let want = buf.len().min(self.chunk);
        let n = match &self.data {
            Data::Bytes(b) => {
                let start = (self.pos as usize).min(b.len());
                let n = want.min(b.len() - start);
                buf[..n].copy_from_slice(&b[start..start + n]);
                n
            }
            Data::Zeros(total) => {
                let n = (*total - self.pos).min(want as u64) as usize;
                buf[..n].fill(0);
                n
            }
            Data::Corrupt(good) => {
                let start = self.pos as usize;
                if start >= good.len() {
                    return Err(io::Error::other(ArchiveError::Corrupt {
                        archive: "/mem.zip".into(),
                        detail: "CRC mismatch".into(),
                    }));
                }
                let n = want.min(good.len() - start);
                buf[..n].copy_from_slice(&good[start..start + n]);
                n
            }
            Data::PanicAfter(good) => {
                let start = self.pos as usize;
                if start >= good.len() {
                    panic!("decoder panic (test)");
                }
                let n = want.min(good.len() - start);
                buf[..n].copy_from_slice(&good[start..start + n]);
                n
            }
            Data::Fatal(good, kind) => {
                let start = self.pos as usize;
                if start >= good.len() {
                    return Err(io::Error::other(kind.error()));
                }
                let n = want.min(good.len() - start);
                buf[..n].copy_from_slice(&good[start..start + n]);
                n
            }
        };
        self.pos += n as u64;
        *self.base.lock().unwrap() += n as u64;
        Ok(n)
    }
}

impl ArchiveReader for MemReader {
    fn index(&self) -> &Arc<ArchiveIndex> {
        &self.index
    }

    fn visit(
        &mut self,
        ids: &[EntryId],
        cancel: &CancelToken,
        f: &mut VisitFn<'_>,
    ) -> Result<(), ArchiveError> {
        if let Some(f) = &self.visit_error {
            return Err(f());
        }
        for id in ids {
            match self.index.get(*id) {
                None => return Err(ArchiveError::NoSuchEntry(id.0)),
                Some(e) if !e.is_safe() => {
                    return Err(ArchiveError::Unsafe {
                        path: e.path.clone(),
                        reason: match e.safety {
                            EntrySafety::Unsafe(r) => r,
                            EntrySafety::Safe => unreachable!(),
                        },
                    });
                }
                Some(_) => {}
            }
        }
        let mut sorted = ids.to_vec();
        sorted.sort();
        let delivered = Arc::new(Mutex::new(0u64));
        for id in sorted {
            if cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let entry = self.index.get(id).cloned().unwrap();
            let outs = match &self.tamper {
                Some(t) => t(&entry),
                None => vec![entry],
            };
            for out in outs {
                let data = self
                    .datas
                    .get(&id)
                    .cloned()
                    .unwrap_or(Data::Bytes(Vec::new()));
                let mut stream = Stream {
                    data,
                    pos: 0,
                    chunk: self.chunk,
                    cancel: cancel.clone(),
                    base: Arc::clone(&delivered),
                    on_read: self.on_read.clone(),
                };
                f(&out, &mut stream)?;
            }
        }
        Ok(())
    }
}

/// An entry exactly as the index has it, but with another path and `Safe`: what a lying reader
/// would hand to the job.
pub fn forged(entry: &ArchiveEntry, path: impl AsRef<std::ffi::OsStr>) -> ArchiveEntry {
    let mut e = entry.clone();
    e.path = PathBuf::from(path.as_ref());
    e.safety = EntrySafety::Safe;
    e
}

// ------------------------------------------------------------------------------- running

pub struct Run {
    pub result: Result<(), JobError>,
    pub report: Report,
    pub rec: Rec,
}

impl Run {
    pub fn ok(&self) -> &Report {
        assert!(self.result.is_ok(), "job failed: {:?}", self.result);
        &self.report
    }

    pub fn warnings(&self) -> Vec<String> {
        self.report
            .warnings
            .iter()
            .map(|w| w.message.clone())
            .collect()
    }

    pub fn questions(&self) -> usize {
        self.rec.lock().unwrap().questions.len()
    }
}

pub fn conflict(action: ConflictAnswer, all: bool) -> Option<Answer> {
    Some(Answer::Conflict {
        action,
        apply_to_all: all,
    })
}

/// Run with a fixed policy: conflicts -> `c`, errors -> `ErrorAction::Skip`.
pub fn run_policy(
    reader: &mut dyn ArchiveReader,
    entries: &[&str],
    dest: &Path,
    opts: &ExtractOpts,
    c: ConflictAnswer,
) -> Run {
    let (sink, rec) = ScriptedSink::policy(c, ErrorAction::Skip, false);
    run_with_ctx(
        JobCtx::simple(Arc::new(RealFs), sink),
        rec,
        reader,
        entries,
        dest,
        opts,
    )
}

/// Run where nobody may be asked (any question aborts, so asserting `questions() == 0` is implied
/// by `Ok`).
pub fn run_plain(
    reader: &mut dyn ArchiveReader,
    entries: &[&str],
    dest: &Path,
    opts: &ExtractOpts,
) -> Run {
    let (sink, rec) = ScriptedSink::refusing();
    run_with_ctx(
        JobCtx::simple(Arc::new(RealFs), sink),
        rec,
        reader,
        entries,
        dest,
        opts,
    )
}

pub fn run_on(
    fs: Arc<dyn Fs>,
    reader: &mut dyn ArchiveReader,
    entries: &[&str],
    dest: &Path,
    opts: &ExtractOpts,
    c: ConflictAnswer,
    e: ErrorAction,
) -> Run {
    let (sink, rec) = ScriptedSink::policy(c, e, false);
    run_with_ctx(JobCtx::simple(fs, sink), rec, reader, entries, dest, opts)
}

/// Run with a context whose cancel token the caller holds.
pub fn run_cancellable(
    cancel: &CancelToken,
    reader: &mut dyn ArchiveReader,
    entries: &[&str],
    dest: &Path,
    opts: &ExtractOpts,
) -> Run {
    let (sink, rec) = ScriptedSink::policy(ConflictAnswer::Skip, ErrorAction::Skip, false);
    let ctx = JobCtx::new(
        Arc::new(RealFs),
        cancel.clone(),
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    run_with_ctx(ctx, rec, reader, entries, dest, opts)
}

/// [`run_plain`] with selections given as paths (non-UTF-8 names).
pub fn run_paths(
    reader: &mut dyn ArchiveReader,
    entries: &[PathBuf],
    dest: &Path,
    opts: &ExtractOpts,
) -> Run {
    let (sink, rec) = ScriptedSink::refusing();
    run_ctx_paths(
        JobCtx::simple(Arc::new(RealFs), sink),
        rec,
        reader,
        entries,
        dest,
        opts,
    )
}

pub fn run_with_ctx(
    ctx: JobCtx,
    rec: Rec,
    reader: &mut dyn ArchiveReader,
    entries: &[&str],
    dest: &Path,
    opts: &ExtractOpts,
) -> Run {
    let entries: Vec<PathBuf> = entries.iter().map(PathBuf::from).collect();
    run_ctx_paths(ctx, rec, reader, &entries, dest, opts)
}

pub fn run_ctx_paths(
    mut ctx: JobCtx,
    rec: Rec,
    reader: &mut dyn ArchiveReader,
    entries: &[PathBuf],
    dest: &Path,
    opts: &ExtractOpts,
) -> Run {
    // The runner does this for `JobSpec::Extract`; direct callers must do it themselves.
    ctx.set_conflict_policy(opts.on_conflict);
    let result = extract_from_reader(&mut ctx, reader, entries, dest, opts);
    ctx.finish_progress();
    Run {
        result,
        report: ctx.take_report(),
        rec,
    }
}

pub fn is_cancelled(r: &Run) -> bool {
    matches!(r.result, Err(JobError::Cancelled))
}

pub fn fail_text(r: &Run) -> String {
    match &r.result {
        Err(e) => e.to_string(),
        Ok(()) => panic!("expected the job to fail"),
    }
}

// ---------------------------------------------------------------------------- assertions

/// Every path below `dir` (not following symlinks), sorted, relative.
pub fn snapshot(dir: &Path) -> Vec<(PathBuf, String)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let md = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().to_path_buf();
            let desc = if md.is_dir() {
                "dir".to_string()
            } else if md.file_type().is_symlink() {
                format!("link -> {}", std::fs::read_link(&p).unwrap().display())
            } else {
                format!("file {} bytes", md.len())
            };
            out.push((rel, desc));
            if md.is_dir() {
                walk(root, &p, out);
            }
        }
    }
    let mut v = Vec::new();
    walk(dir, dir, &mut v);
    v.sort();
    v
}

pub fn part_files_in(dir: &Path) -> Vec<PathBuf> {
    snapshot(dir)
        .into_iter()
        .filter(|(p, _)| p.to_string_lossy().contains(PART_MARKER))
        .map(|(p, _)| dir.join(p))
        .collect()
}

pub fn assert_no_part_files(dir: &Path) {
    let parts = part_files_in(dir);
    assert!(parts.is_empty(), "temp files survived: {parts:?}");
}

pub fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

pub fn perm(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().mode() & 0o7777
}

pub fn mtime_ns(p: &Path) -> i128 {
    let md = std::fs::symlink_metadata(p).unwrap();
    i128::from(md.mtime()) * 1_000_000_000 + i128::from(md.mtime_nsec())
}

/// A name that does not exist (not even as a dangling symlink).
pub fn assert_absent(p: &Path) {
    assert!(
        std::fs::symlink_metadata(p).is_err(),
        "{} must not exist",
        p.display()
    );
}

/// Data-safety core: `path` is absent or byte-exact.
pub fn assert_absent_or_exact(path: &Path, content: &[u8]) {
    match std::fs::read(path) {
        Ok(b) => assert!(
            b == content,
            "{} exists but is truncated or wrong ({} of {} bytes)",
            path.display(),
            b.len(),
            content.len()
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

pub fn opts() -> ExtractOpts {
    ExtractOpts::default()
}

/// Deterministic pseudo-random bytes.
pub fn bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

pub fn question_conflicts(rec: &Rec) -> Vec<Question> {
    rec.lock()
        .unwrap()
        .questions
        .iter()
        .filter(|q| matches!(q, Question::Conflict { .. }))
        .cloned()
        .collect()
}
