//! T8.5 adversarial tests for the content-compare job (T8.1b, F27 content mode).
//!
//! Complements `compare_content.rs` with the hostile end of the input space: sockets and
//! devices that must never be opened, symlink loops, size races between `lstat` and open
//! (simulated by a fake `Fs`), cross-device pairs, cancel at every pair index, buffer-size
//! extremes and a large batch.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ofm_core::CancelToken;
use ofm_jobs::fs::TrashError;
use ofm_jobs::{
    CompareContentOpts, Fs, FsFile, FsStat, JobCtx, JobSpec, Outcome, PairResult, PauseGate,
    RealFs, ResultSink, ScriptedSink, SkipReason, SystemClock, run_job, run_sync,
};
use ofm_testkit::temp::TempDir;

type Collected = Arc<Mutex<Vec<(usize, PairResult)>>>;

fn collector() -> (CompareContentOpts, Collected) {
    let got: Collected = Arc::default();
    let g = Arc::clone(&got);
    let opts = CompareContentOpts {
        on_result: Some(ResultSink::new(move |i, r| {
            g.lock().unwrap().push((i, r.clone()));
        })),
        ..CompareContentOpts::default()
    };
    (opts, got)
}

fn noise(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ ((i >> 8) as u8))
        .collect()
}

fn put(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

/// Run through `run_sync` and return the outcome plus the streamed results; the stream must be
/// in pair order with exactly one entry per pair.
fn compare_on(fs: Arc<dyn Fs>, pairs: Vec<(PathBuf, PathBuf)>) -> (Outcome, Vec<PairResult>) {
    let (opts, got) = collector();
    let (sink, rec) = ScriptedSink::refusing();
    let count = pairs.len();
    let out = run_sync(&JobSpec::CompareContent { pairs, opts }, fs, sink);
    assert!(rec.lock().unwrap().questions.is_empty());
    let v = got.lock().unwrap().clone();
    assert!(v.iter().enumerate().all(|(n, (i, _))| n == *i), "{v:?}");
    if matches!(out, Outcome::Ok(_)) {
        assert_eq!(v.len(), count, "one streamed result per pair");
    }
    (out, v.into_iter().map(|(_, r)| r).collect())
}

fn compare(pairs: Vec<(PathBuf, PathBuf)>) -> (Outcome, Vec<PairResult>) {
    compare_on(Arc::new(RealFs), pairs)
}

fn one(a: &Path, b: &Path) -> PairResult {
    let (out, mut r) = compare(vec![(a.to_path_buf(), b.to_path_buf())]);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(r.len(), 1);
    r.remove(0)
}

// --- Fake fs: deterministic size races -------------------------------------------------------

/// In-memory stand-in for a file whose content/size changed between `lstat` (real, on the path)
/// and `open_read`/`fstat` (this). Reads are deliberately short (<= 3 bytes) to stress the
/// read-loop of the comparison.
struct FakeFile {
    data: Vec<u8>,
    pos: usize,
    stat_size: u64,
    fd: OwnedFd,
}

impl FakeFile {
    fn new(data: Vec<u8>, stat_size: u64) -> Self {
        let fd = rustix::fs::open(
            "/dev/null",
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        FakeFile {
            data,
            pos: 0,
            stat_size,
            fd,
        }
    }
}

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

impl FsFile for FakeFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(3).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        unsupported()
    }
    fn stat(&self) -> io::Result<FsStat> {
        Ok(FsStat {
            dev: 0xF0,
            ino: 0xF0,
            mode: 0o100_644,
            nlink: 1,
            size: self.stat_size,
            blocks: self.stat_size.div_ceil(512),
            uid: 0,
            gid: 0,
            atime_ns: 0,
            mtime_ns: 0,
        })
    }
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
    fn try_clone_from(&mut self, _: &dyn FsFile) -> io::Result<bool> {
        unsupported()
    }
    fn copy_range_from(&mut self, _: &mut dyn FsFile, _: u64) -> io::Result<Option<u64>> {
        unsupported()
    }
    fn seek_data(&mut self, _: u64) -> io::Result<Option<u64>> {
        unsupported()
    }
    fn seek_hole(&mut self, _: u64) -> io::Result<u64> {
        unsupported()
    }
    fn seek(&mut self, _: u64) -> io::Result<()> {
        unsupported()
    }
    fn set_len(&mut self, _: u64) -> io::Result<()> {
        unsupported()
    }
    fn set_mode(&mut self, _: u32) -> io::Result<()> {
        unsupported()
    }
    fn set_times(&mut self, _: i128, _: i128) -> io::Result<()> {
        unsupported()
    }
    fn set_owner(&mut self, _: Option<u32>, _: Option<u32>) -> io::Result<()> {
        unsupported()
    }
    fn sync(&mut self) -> io::Result<()> {
        unsupported()
    }
}

/// Delegates everything to [`RealFs`] except `open_read` on registered paths, which returns a
/// [`FakeFile`]: the on-disk file (and its `lstat`) is real, what the opener sees is not.
struct SwapFs {
    inner: RealFs,
    fakes: HashMap<PathBuf, (Vec<u8>, u64)>,
}

impl SwapFs {
    fn new() -> Self {
        SwapFs {
            inner: RealFs,
            fakes: HashMap::new(),
        }
    }
    fn fake(mut self, path: &Path, data: Vec<u8>, stat_size: u64) -> Self {
        self.fakes.insert(path.to_path_buf(), (data, stat_size));
        self
    }
}

macro_rules! delegate {
    ($($name:ident ( &self $(, $a:ident : $t:ty )* ) -> $r:ty ;)*) => {$(
        fn $name(&self, $( $a: $t ),*) -> $r { self.inner.$name($( $a ),*) }
    )*};
}

impl Fs for SwapFs {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        if let Some((data, size)) = self.fakes.get(path) {
            return Ok(Box::new(FakeFile::new(data.clone(), *size)));
        }
        self.inner.open_read(path)
    }
    delegate! {
        open_read_follow(&self, path: &Path) -> io::Result<Box<dyn FsFile>>;
        create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>>;
        stat(&self, path: &Path) -> io::Result<FsStat>;
        lstat(&self, path: &Path) -> io::Result<FsStat>;
        read_dir(&self, path: &Path) -> io::Result<Vec<OsString>>;
        mkdir(&self, path: &Path, mode: u32) -> io::Result<()>;
        rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()>;
        rename_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
        unlink(&self, path: &Path) -> io::Result<()>;
        rmdir(&self, path: &Path) -> io::Result<()>;
        symlink(&self, target: &Path, link: &Path) -> io::Result<()>;
        hard_link(&self, from: &Path, to: &Path) -> io::Result<()>;
        read_link(&self, path: &Path) -> io::Result<PathBuf>;
        set_mode(&self, path: &Path, mode: u32) -> io::Result<()>;
        set_times(&self, path: &Path, atime_ns: i128, mtime_ns: i128, follow: bool) -> io::Result<()>;
        set_owner(&self, path: &Path, uid: Option<u32>, gid: Option<u32>, follow: bool) -> io::Result<()>;
        sync_dir(&self, path: &Path) -> io::Result<()>;
        list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>>;
        get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>>;
        set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()>;
        remove_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<()>;
        trash(&self, path: &Path) -> Result<(), TrashError>;
        is_root(&self) -> bool;
    }
}

// --- Special files: never opened -------------------------------------------------------------

/// A unix-domain socket on either side is `Skipped(SpecialFile)` without ever being opened.
#[test]
fn socket_pair_is_skipped_never_opened() {
    let t = TempDir::new().unwrap();
    let sock = t.path().join("sock");
    let Ok(_listener) = std::os::unix::net::UnixListener::bind(&sock) else {
        eprintln!("SKIP: cannot bind a unix socket here");
        return;
    };
    let f = put(t.path(), "f", b"data");
    assert_eq!(one(&sock, &f), PairResult::Skipped(SkipReason::SpecialFile));
    assert_eq!(one(&f, &sock), PairResult::Skipped(SkipReason::SpecialFile));
    assert_eq!(
        one(&sock, &sock),
        PairResult::Skipped(SkipReason::SpecialFile)
    );
}

/// Character devices (`/dev/null`) are refused by the lstat pre-check: skipped, not read.
/// Comparing a device against itself must NOT take the same-inode shortcut.
#[test]
fn char_device_is_skipped_even_against_itself() {
    if !Path::new("/dev/null").exists() {
        eprintln!("SKIP: no /dev/null");
        return;
    }
    let nul = PathBuf::from("/dev/null");
    assert_eq!(
        one(&nul, &nul),
        PairResult::Skipped(SkipReason::SpecialFile)
    );
}

// --- Symlink loops ----------------------------------------------------------------------------

/// Symlink loops (a <-> b, s -> s) compare by target string only: never followed, never hanging,
/// decided well within the deadline.
#[test]
fn symlink_loops_compare_targets_without_hanging() {
    let t = TempDir::new().unwrap();
    let (l, r) = (t.path().join("l"), t.path().join("r"));
    std::fs::create_dir_all(&l).unwrap();
    std::fs::create_dir_all(&r).unwrap();
    for d in [&l, &r] {
        std::os::unix::fs::symlink("b", d.join("a")).unwrap();
        std::os::unix::fs::symlink("a", d.join("b")).unwrap();
        std::os::unix::fs::symlink("s", d.join("s")).unwrap();
    }
    let f = put(&r, "f", b"x");
    let start = Instant::now();
    // Loop members with equal target strings are equal...
    assert_eq!(one(&l.join("a"), &r.join("a")), PairResult::Equal);
    assert_eq!(one(&l.join("s"), &r.join("s")), PairResult::Equal);
    // ...different target strings differ, and a loop against a file differs.
    assert_eq!(one(&l.join("a"), &r.join("b")), PairResult::Differs);
    assert_eq!(one(&l.join("a"), &f), PairResult::Differs);
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "symlink loops must not hang"
    );
}

// --- Deterministic size races (via SwapFs) ------------------------------------------------------

/// The file grew between `lstat` and the fstat on the open descriptor: sizes disagree -> Differs.
#[test]
fn grown_between_lstat_and_open_differs() {
    let t = TempDir::new().unwrap();
    let data = noise(10_000, 7);
    let a = put(t.path(), "a", &data);
    let b = put(t.path(), "b", &data);
    let fs = SwapFs::new().fake(&a, data.clone(), data.len() as u64 + 100);
    let (out, r) = compare_on(Arc::new(fs), vec![(a, b)]);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(r, vec![PairResult::Differs]);
}

/// Both sides shrank to the same shorter prefix while reading: nothing trustworthy to compare,
/// so a per-pair error ("changed while comparing") naming the left path — not Equal, not Differs.
#[test]
fn shrunk_identically_on_both_sides_is_changed_error() {
    let t = TempDir::new().unwrap();
    let data = noise(10_000, 11);
    let a = put(t.path(), "a", &data);
    let b = put(t.path(), "b", &data);
    let short = data[..data.len() - 4].to_vec();
    let fs = SwapFs::new()
        .fake(&a, short.clone(), data.len() as u64)
        .fake(&b, short, data.len() as u64);
    let (out, r) = compare_on(Arc::new(fs), vec![(a.clone(), b)]);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    match &r[..] {
        [PairResult::Error(e)] => {
            assert_eq!(e.path, a);
            assert!(e.message.contains("changed while comparing"), "{e:?}");
        }
        other => panic!("expected changed-while-comparing error, got {other:?}"),
    }
}

// --- Sparseness and devices -----------------------------------------------------------------

/// The same content on two different filesystems (/dev/shm vs the temp dir) compares Equal;
/// one flipped byte compares Differs. Skips politely when there is no second filesystem.
#[test]
fn dev_shm_versus_tmp_cross_device() {
    let Ok(shm) = TempDir::in_dir(Path::new("/dev/shm")) else {
        eprintln!("SKIP: /dev/shm not writable");
        return;
    };
    let t = TempDir::new().unwrap();
    let data = noise(50_000, 17);
    let probe = put(shm.path(), "probe", &data);
    let a = put(t.path(), "a", &data);
    if RealFs.lstat(&a).unwrap().dev == RealFs.lstat(&probe).unwrap().dev {
        eprintln!("SKIP: temp dir and /dev/shm are the same filesystem");
        return;
    }
    assert_eq!(one(&a, &probe), PairResult::Equal);
    let mut other = data.clone();
    other[25_000] ^= 0xFF;
    std::fs::write(&probe, &other).unwrap();
    assert_eq!(one(&a, &probe), PairResult::Differs);
}

// --- Scale, cancel sweep, buffer extremes -----------------------------------------------------

/// 2000 pairs, every 7th differing: the full result vector is exact and in order.
#[test]
fn two_thousand_pairs_are_exact_and_in_order() {
    let t = TempDir::new().unwrap();
    let (l, r) = (t.path().join("l"), t.path().join("r"));
    std::fs::create_dir_all(&l).unwrap();
    std::fs::create_dir_all(&r).unwrap();
    let n = 2_000u32;
    let mut pairs = Vec::with_capacity(n as usize);
    for i in 0..n {
        let mut data = i.to_le_bytes().to_vec();
        let lb = put(&l, &format!("{i:06}"), &data);
        if i % 7 == 3 {
            data[0] ^= 1; // differing twin
        }
        let rb = put(&r, &format!("{i:06}"), &data);
        pairs.push((lb, rb));
    }
    let (out, results) = compare(pairs);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(results.len(), n as usize);
    for (i, r) in results.iter().enumerate() {
        let expect = if i as u32 % 7 == 3 {
            PairResult::Differs
        } else {
            PairResult::Equal
        };
        assert_eq!(*r, expect, "pair {i}");
    }
}

/// Cancelling after the k-th streamed result (for every k) delivers exactly that prefix. Before
/// the last pair the outcome is Cancelled; a cancel raised by the LAST result is never observed
/// (nothing is left to do), so the job reports Ok.
#[test]
fn cancel_at_every_pair_index_delivers_exact_prefix() {
    let t = TempDir::new().unwrap();
    let data = noise(2_000, 19);
    let n = 6;
    let mut pairs = Vec::new();
    for i in 0..n {
        pairs.push((
            put(t.path(), &format!("a{i}"), &data),
            put(t.path(), &format!("b{i}"), &data),
        ));
    }
    for k in 0..n {
        let (sink, _) = ScriptedSink::refusing();
        let token = CancelToken::new();
        let mut ctx = JobCtx::new(
            Arc::new(RealFs),
            token.clone(),
            PauseGate::new(),
            Box::new(sink),
            Arc::new(SystemClock::new()),
        );
        let seen = Arc::new(AtomicUsize::new(0));
        let (s2, t2) = (Arc::clone(&seen), token.clone());
        let opts = CompareContentOpts {
            on_result: Some(ResultSink::new(move |i, _| {
                s2.fetch_add(1, Ordering::SeqCst);
                if i == k {
                    t2.cancel();
                }
            })),
            ..CompareContentOpts::default()
        };
        let out = run_job(
            &JobSpec::CompareContent {
                pairs: pairs.clone(),
                opts,
            },
            &mut ctx,
        );
        if k + 1 < n {
            assert!(matches!(out, Outcome::Cancelled(_)), "k={k}: {out:?}");
        } else {
            assert!(matches!(out, Outcome::Ok(_)), "k={k}: {out:?}");
        }
        assert_eq!(seen.load(Ordering::SeqCst), k + 1, "k={k}");
    }
}

/// `buffer_size` 0 and `usize::MAX` are clamped into the supported range and produce correct
/// results (a runaway buffer config must not break comparison or allocate absurdly).
#[test]
fn buffer_size_extremes_are_clamped() {
    let t = TempDir::new().unwrap();
    let data = noise(300_000, 21);
    let mut other = data.clone();
    other[299_999] ^= 1;
    let a = put(t.path(), "a", &data);
    let b = put(t.path(), "b", &data);
    let c = put(t.path(), "c", &other);
    for buffer_size in [0usize, usize::MAX] {
        let (opts, got) = collector();
        let opts = CompareContentOpts {
            buffer_size,
            ..opts
        };
        let (sink, _) = ScriptedSink::refusing();
        let out = run_sync(
            &JobSpec::CompareContent {
                pairs: vec![(a.clone(), b.clone()), (a.clone(), c.clone())],
                opts,
            },
            Arc::new(RealFs),
            sink,
        );
        assert!(matches!(out, Outcome::Ok(_)), "buf={buffer_size}: {out:?}");
        let v = got.lock().unwrap().clone();
        assert_eq!(
            v.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>(),
            vec![PairResult::Equal, PairResult::Differs],
            "buf={buffer_size}"
        );
    }
}

/// An empty pair list is a successful no-op with no streamed results.
#[test]
fn empty_pair_list_is_a_clean_no_op() {
    let (out, results) = compare(Vec::new());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(results.is_empty());
}
