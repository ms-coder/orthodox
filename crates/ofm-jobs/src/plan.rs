//! Plan phase of copy/move (T3.1): parse the request, scan sources for totals, refuse
//! destination-inside-source, make sure the destination directory exists, and compute names
//! (rename mask, `name (N).ext` for "keep both" answers).
//!
//! "Same file" (copy into the directory the source lives in, hardlinks, bind mounts) is not
//! decided here but per item, by dev+ino, when the destination name is probed
//! (`copy::resolve_target` -> `ConflictKind::SameFile`): that also catches cases a
//! directory-level comparison would miss.
//!
//! Nothing in this module writes to the file system except [`ensure_dir`].

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use ofm_core::Location;
use ofm_core::mask::RenameMask;

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::fs::{FileType, Fs, FsStat};

/// Longest file name Linux file systems accept (`NAME_MAX`).
pub const NAME_MAX: usize = 255;

/// Upper bound for the `name (N)` probe and for the parent walk of [`check_destination`].
const MAX_PROBE: u32 = 100_000;

/// `ELOOP`: too many levels of symbolic links.
const ELOOP: i32 = 40;

/// Totals found by [`scan`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Non-directory entries that will be copied (regular files and symlinks; special files are
    /// skipped by the copy and not counted).
    pub files: u64,
    /// Directories.
    pub dirs: u64,
    /// Sum of regular file sizes.
    pub bytes: u64,
}

/// Resolve job sources to local paths. v1.0/M3 copies and moves local items only; archive entries
/// (M6) and list locations (resolved to real paths by the UI before submitting) are
/// `JobError::Unsupported` here. Relative paths are `JobError::Invalid` (the UI resolves them).
pub fn local_paths(sources: &[Location]) -> Result<Vec<PathBuf>, JobError> {
    if sources.is_empty() {
        return Err(JobError::Invalid("no sources".into()));
    }
    sources
        .iter()
        .map(|s| match s {
            Location::Local(p) if p.is_absolute() => Ok(p.clone()),
            Location::Local(p) => Err(JobError::Invalid(format!(
                "source path is not absolute: {}",
                p.display()
            ))),
            other => Err(JobError::Unsupported(format!(
                "copy/move from {}",
                other.display_lossy()
            ))),
        })
        .collect()
}

/// Parse `CopyOpts::rename_mask` (`None` = keep names). An invalid mask is `JobError::Invalid`.
pub fn parse_mask(mask: Option<&str>) -> Result<Option<RenameMask>, JobError> {
    mask.map(|m| RenameMask::parse(m).map_err(|e| JobError::Invalid(format!("rename mask: {e}"))))
        .transpose()
}

/// Whether `name` is usable as a single directory entry name.
pub fn is_valid_name(name: &OsStr) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b != b"."
        && b != b".."
        && b.len() <= NAME_MAX
        && !b.iter().any(|&c| c == b'/' || c == 0)
}

/// Destination names of all top-level sources (rename mask applied), validated before any I/O:
/// a mask yielding `""`, `.`, `..`, a `/` or more than 255 bytes is `JobError::Invalid`, and so
/// are two sources with the same destination name (T3.6 B1: `*.bak` on `a.txt` + `a.md`, or two
/// `f` from different folders of a list panel), which would otherwise meet as a conflict of the
/// job with its own output. Names are compared bytewise; collisions that only a case-insensitive
/// destination creates are caught per item by `copy::resolve_target`.
pub fn target_names(
    sources: &[PathBuf],
    mask: Option<&RenameMask>,
) -> Result<Vec<OsString>, JobError> {
    let mut seen: HashMap<OsString, &Path> = HashMap::new();
    let mut out = Vec::with_capacity(sources.len());
    for s in sources {
        let name = target_name(s, mask);
        if !is_valid_name(&name) {
            return Err(JobError::Invalid(format!(
                "invalid destination name '{}' for {}",
                name.to_string_lossy(),
                s.display()
            )));
        }
        if let Some(first) = seen.insert(name.clone(), s) {
            return Err(JobError::Invalid(format!(
                "{} and {} would both be written as '{}'",
                first.display(),
                s.display(),
                name.to_string_lossy()
            )));
        }
        out.push(name);
    }
    Ok(out)
}

/// Count files/dirs/bytes under `sources` (never follows symlinks unless `follow_symlinks`; guards
/// against symlink loops when following). Reports through `ctx.add_total` (so the UI shows a
/// growing total with `planning = true`) and checks `ctx.checkpoint()` per directory. Unreadable
/// entries are not errors here: the execute phase reports them (with retry/skip/abort).
pub fn scan(
    ctx: &mut JobCtx,
    sources: &[PathBuf],
    follow_symlinks: bool,
) -> Result<Totals, JobError> {
    ctx.set_planning(true);
    count(ctx, sources, follow_symlinks, true)
}

/// The walk behind [`scan`]; `report = false` counts silently (a same-device move that fell back
/// to copying adds its totals itself).
pub(crate) fn count(
    ctx: &mut JobCtx,
    sources: &[PathBuf],
    follow: bool,
    report: bool,
) -> Result<Totals, JobError> {
    let fs = ctx.fs();
    let mut totals = Totals::default();
    let mut visited: HashSet<(u64, u64)> = HashSet::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    for src in sources {
        ctx.checkpoint()?;
        let before = totals;
        classify(&*fs, src, follow, &mut stack, &mut visited, &mut totals);
        if report {
            ctx.add_total(totals.files - before.files, totals.bytes - before.bytes);
        }
    }
    while let Some(dir) = stack.pop() {
        ctx.checkpoint()?;
        let names = match fs.read_dir(&dir) {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!(path = %dir.display(), error = %e, "scan: cannot list");
                continue;
            }
        };
        let before = totals;
        for name in names {
            classify(
                &*fs,
                &dir.join(name),
                follow,
                &mut stack,
                &mut visited,
                &mut totals,
            );
        }
        if report {
            ctx.add_total(totals.files - before.files, totals.bytes - before.bytes);
        }
    }
    Ok(totals)
}

/// `lstat`, or `stat` when following (a dangling or looping link is then the link itself).
pub(crate) fn stat_maybe_follow(fs: &dyn Fs, path: &Path, follow: bool) -> io::Result<FsStat> {
    if !follow {
        return fs.lstat(path);
    }
    match fs.stat(path) {
        Ok(st) => Ok(st),
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(ELOOP) => {
            fs.lstat(path)
        }
        Err(e) => Err(e),
    }
}

fn classify(
    fs: &dyn Fs,
    path: &Path,
    follow: bool,
    stack: &mut Vec<PathBuf>,
    visited: &mut HashSet<(u64, u64)>,
    totals: &mut Totals,
) {
    let st = match stat_maybe_follow(fs, path, follow) {
        Ok(st) => st,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "scan: cannot stat");
            return;
        }
    };
    match st.file_type() {
        FileType::Dir => {
            totals.dirs += 1;
            // Without following there are no directory cycles (no directory hardlinks).
            if !follow || visited.insert((st.dev, st.ino)) {
                stack.push(path.to_path_buf());
            }
        }
        FileType::File => {
            totals.files += 1;
            totals.bytes = totals.bytes.saturating_add(st.size);
        }
        FileType::Symlink => totals.files += 1,
        FileType::Other => {}
    }
}

/// Refuse copying/moving a directory into itself or its own subtree (`DestInsideSource`).
///
/// The check is physical, not lexical: the chain of `(dev, ino)` of the destination directory (or
/// its nearest existing ancestor when it does not exist yet) and of all its parents (`..`, as the
/// kernel resolves it) is compared with every source directory, so symlinked or differently
/// spelled paths are caught too. `follow` = sources that are symlinks to directories count as
/// those directories (copy with `follow_symlinks`).
///
/// Also fails when `dest_dir` is relative, or exists and is not a directory.
pub fn check_destination(
    ctx: &mut JobCtx,
    sources: &[PathBuf],
    dest_dir: &Path,
    follow: bool,
) -> Result<(), JobError> {
    if !dest_dir.is_absolute() {
        return Err(JobError::Invalid(format!(
            "destination is not absolute: {}",
            dest_dir.display()
        )));
    }
    let fs = ctx.fs();
    // Nearest existing ancestor (the destination itself when it exists).
    let mut existing = dest_dir.to_path_buf();
    let st = loop {
        match fs.stat(&existing) {
            Ok(st) => break st,
            Err(e) if e.kind() == io::ErrorKind::NotFound => match existing.parent() {
                Some(p) => existing = p.to_path_buf(),
                None => return Err(JobError::io(dest_dir, e)),
            },
            Err(e) => return Err(JobError::io(existing, e)),
        }
    };
    if !st.is_dir() {
        return Err(JobError::io(
            existing,
            io::Error::from(io::ErrorKind::NotADirectory),
        ));
    }
    let mut chain: HashSet<(u64, u64)> = HashSet::new();
    let mut cur = existing;
    let mut cur_st = st;
    for _ in 0..MAX_PROBE {
        chain.insert((cur_st.dev, cur_st.ino));
        let parent = cur.join("..");
        let parent_st = fs.stat(&parent).map_err(|e| JobError::io(&parent, e))?;
        if (parent_st.dev, parent_st.ino) == (cur_st.dev, cur_st.ino) {
            break; // reached "/"
        }
        cur = parent;
        cur_st = parent_st;
    }
    for src in sources {
        ctx.checkpoint()?;
        match stat_maybe_follow(&*fs, src, follow) {
            Ok(s) if s.is_dir() && chain.contains(&(s.dev, s.ino)) => {
                return Err(JobError::DestInsideSource(dest_dir.to_path_buf()));
            }
            Ok(_) => {}
            // A missing source is reported (with retry/skip) by the execute phase.
            Err(e) => tracing::debug!(path = %src.display(), error = %e, "check: cannot stat"),
        }
    }
    Ok(())
}

/// Create `dir` and missing parents (like `mkdir -p`, mode 0777 minus umask). An existing
/// directory is fine; anything else fails the job with the offending path.
pub fn ensure_dir(ctx: &mut JobCtx, dir: &Path) -> Result<(), JobError> {
    let fs = ctx.fs();
    let mut missing: Vec<&Path> = Vec::new();
    let mut cur = Some(dir);
    while let Some(p) = cur {
        match fs.stat(p) {
            Ok(st) if st.is_dir() => break,
            Ok(_) => {
                return Err(JobError::io(
                    p,
                    io::Error::from(io::ErrorKind::NotADirectory),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                missing.push(p);
                cur = p.parent();
            }
            Err(e) => return Err(JobError::io(p, e)),
        }
    }
    for p in missing.into_iter().rev() {
        ctx.checkpoint()?;
        match fs.mkdir(p, 0o777) {
            Ok(()) => tracing::debug!(path = %p.display(), "created destination directory"),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(JobError::io(p, e)),
        }
    }
    Ok(())
}

/// Destination name for a top-level source: apply the rename mask if any.
pub fn target_name(source: &Path, mask: Option<&RenameMask>) -> OsString {
    let name = source.file_name().unwrap_or(source.as_os_str());
    match mask {
        Some(m) => m.apply(name),
        None => name.to_owned(),
    }
}

/// Split a name into stem and extension for numbering: the extension starts at the last dot,
/// unless that dot is the first byte (`.bashrc` has no extension).
fn split_ext(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().rposition(|&c| c == b'.') {
        Some(p) if p > 0 => (&name[..p], &name[p..]),
        _ => (name, &[]),
    }
}

/// `stem (n)ext`, shortening the stem (bytes) so the result stays within [`NAME_MAX`].
pub(crate) fn numbered_name(name: &OsStr, n: u32) -> OsString {
    let (mut stem, mut ext) = split_ext(name.as_bytes());
    let tag = format!(" ({n})");
    if tag.len() + ext.len() >= NAME_MAX {
        // Absurdly long "extension": number the whole name instead.
        stem = name.as_bytes();
        ext = &[];
    }
    let budget = NAME_MAX - tag.len() - ext.len();
    if stem.len() > budget {
        stem = &stem[..budget];
    }
    let mut out = Vec::with_capacity(stem.len() + tag.len() + ext.len());
    out.extend_from_slice(stem);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(ext);
    OsString::from_vec(out)
}

/// First free name `name (N).ext` (N >= 1) in `dir`, probing with `lstat` (ADR-0006: the scheme for
/// `RenameNew`/`RenameExisting`; `.bashrc` -> `.bashrc (1)`, `a.tar.gz` -> `a.tar (1).gz`).
pub fn unique_name(fs: &dyn Fs, dir: &Path, name: &OsStr) -> io::Result<OsString> {
    for n in 1..=MAX_PROBE {
        let candidate = numbered_name(name, n);
        match fs.lstat(&dir.join(&candidate)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!(
        "no free name for '{}' in {}",
        name.to_string_lossy(),
        dir.display()
    )))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ctx::ScriptedSink;
    use crate::fs::RealFs;
    use ofm_testkit::temp::TempDir;
    use ofm_testkit::tree::{FileSpec, TreeSpec};

    fn ctx() -> (JobCtx, Arc<std::sync::Mutex<crate::ctx::Recording>>) {
        let (sink, rec) = ScriptedSink::refusing();
        (JobCtx::simple(Arc::new(RealFs), sink), rec)
    }

    #[test]
    fn local_paths_rejects_non_local_and_empty() {
        assert!(matches!(local_paths(&[]), Err(JobError::Invalid(_))));
        let arch = Location::Archive {
            archive: "/a.zip".into(),
            inner: "x".into(),
        };
        assert!(matches!(
            local_paths(&[arch]),
            Err(JobError::Unsupported(_))
        ));
        assert!(matches!(
            local_paths(&[Location::Local("rel".into())]),
            Err(JobError::Invalid(_))
        ));
        assert_eq!(
            local_paths(&[Location::Local("/a".into())]).unwrap_or_default(),
            vec![PathBuf::from("/a")]
        );
    }

    #[test]
    fn target_name_applies_mask() {
        let m = RenameMask::parse("*.bak").ok();
        assert_eq!(
            target_name(Path::new("/x/a.txt"), m.as_ref()),
            OsString::from("a.bak")
        );
        assert_eq!(
            target_name(Path::new("/x/a.txt"), None),
            OsString::from("a.txt")
        );
    }

    #[test]
    fn target_names_are_validated() {
        let srcs = vec![PathBuf::from("/x/a.txt")];
        for bad in ["..", ".", "a/b"] {
            let m = parse_mask(Some(bad)).unwrap();
            assert!(
                matches!(target_names(&srcs, m.as_ref()), Err(JobError::Invalid(_))),
                "{bad}"
            );
        }
        assert!(matches!(
            target_names(&[PathBuf::from("/")], None),
            Err(JobError::Invalid(_))
        ));
        assert!(matches!(parse_mask(Some("")), Err(JobError::Invalid(_))));
        assert!(matches!(parse_mask(Some("[a]")), Err(JobError::Invalid(_))));
        assert_eq!(
            parse_mask(None).unwrap().map(|m| m.target().to_owned()),
            None
        );
    }

    #[test]
    fn duplicate_target_names_are_invalid() {
        let m = parse_mask(Some("*.bak")).unwrap();
        let srcs = vec![PathBuf::from("/x/a.txt"), PathBuf::from("/x/a.md")];
        assert!(matches!(
            target_names(&srcs, m.as_ref()),
            Err(JobError::Invalid(msg)) if msg.contains("a.bak")
        ));
        assert_eq!(target_names(&srcs, None).unwrap().len(), 2);
        let same = vec![PathBuf::from("/a/f"), PathBuf::from("/b/f")];
        assert!(matches!(
            target_names(&same, None),
            Err(JobError::Invalid(_))
        ));
        // Bytewise: case variants are distinct here (a case-insensitive destination is handled
        // per item at run time).
        let case = vec![PathBuf::from("/a/IMG.JPG"), PathBuf::from("/a/img.jpg")];
        assert_eq!(target_names(&case, None).unwrap().len(), 2);
    }

    #[test]
    fn scan_counts_tree_and_reports_planning_progress() {
        let t = TreeSpec::new()
            .file("a", FileSpec::size(10))
            .dir(
                "d",
                TreeSpec::new()
                    .file("b", FileSpec::size(5))
                    .symlink("l", "b")
                    .dir("e", TreeSpec::new()),
            )
            .build_temp()
            .unwrap();
        let (mut ctx, rec) = ctx();
        let totals = scan(&mut ctx, &[t.path().to_path_buf()], false).unwrap();
        assert_eq!(
            totals,
            Totals {
                files: 3,
                dirs: 3,
                bytes: 15
            }
        );
        let rec = rec.lock().unwrap();
        assert!(rec.progress.first().is_some_and(|p| p.planning));
    }

    #[test]
    fn scan_with_follow_survives_symlink_loops() {
        let t = TreeSpec::new()
            .dir("d", TreeSpec::new().file("f", FileSpec::size(3)))
            .build_temp()
            .unwrap();
        std::os::unix::fs::symlink("..", t.path().join("d/up")).unwrap();
        std::os::unix::fs::symlink("nowhere", t.path().join("d/dangling")).unwrap();
        let (mut ctx, _) = ctx();
        let totals = scan(&mut ctx, &[t.path().join("d")], true).unwrap();
        // d, its parent via `up` (visited once); f counted once; the dangling link as a link.
        assert_eq!(totals.bytes, 3);
        assert_eq!(totals.files, 2);
    }

    #[test]
    fn dest_inside_source_and_same_file_are_refused() {
        let t = TreeSpec::new()
            .dir("src", TreeSpec::new().dir("sub", TreeSpec::new()))
            .file("f", FileSpec::size(1))
            .build_temp()
            .unwrap();
        let src = t.path().join("src");
        let (mut ctx, _) = ctx();
        for dest in [
            src.clone(),
            src.join("sub"),
            src.join("sub/new/deeper"),
            src.join("sub/../sub"),
        ] {
            assert!(
                matches!(
                    check_destination(&mut ctx, std::slice::from_ref(&src), &dest, false),
                    Err(JobError::DestInsideSource(_))
                ),
                "{}",
                dest.display()
            );
        }
        // Through a symlink to the source's subtree.
        std::os::unix::fs::symlink(src.join("sub"), t.path().join("alias")).unwrap();
        assert!(matches!(
            check_destination(
                &mut ctx,
                std::slice::from_ref(&src),
                &t.path().join("alias"),
                false
            ),
            Err(JobError::DestInsideSource(_))
        ));
        // Siblings and the parent are fine; a file as destination is not.
        check_destination(&mut ctx, std::slice::from_ref(&src), t.path(), false).unwrap();
        assert!(matches!(
            check_destination(&mut ctx, &[src], &t.path().join("f"), false),
            Err(JobError::Io { .. })
        ));
        assert!(matches!(
            check_destination(&mut ctx, &[], Path::new("rel"), false),
            Err(JobError::Invalid(_))
        ));
    }

    #[test]
    fn ensure_dir_creates_parents() {
        let t = TempDir::new().unwrap();
        let (mut ctx, _) = ctx();
        let d = t.path().join("a/b/c");
        ensure_dir(&mut ctx, &d).unwrap();
        assert!(d.is_dir());
        ensure_dir(&mut ctx, &d).unwrap();
        std::fs::write(t.path().join("file"), b"x").unwrap();
        assert!(ensure_dir(&mut ctx, &t.path().join("file/x")).is_err());
    }

    #[test]
    fn unique_name_scheme() {
        let t = TreeSpec::new()
            .file(".bashrc", FileSpec::size(1))
            .file("a.tar.gz", FileSpec::size(1))
            .file("a.tar (1).gz", FileSpec::size(1))
            .dir("dir", TreeSpec::new())
            .build_temp()
            .unwrap();
        let fs = RealFs;
        let u = |n: &str| unique_name(&fs, t.path(), OsStr::new(n)).unwrap();
        assert_eq!(u(".bashrc"), OsString::from(".bashrc (1)"));
        assert_eq!(u("a.tar.gz"), OsString::from("a.tar (2).gz"));
        assert_eq!(u("dir"), OsString::from("dir (1)"));
        let long = OsString::from_vec(vec![b'x'; 255]);
        let n = numbered_name(&long, 7);
        assert_eq!(n.len(), 255);
        assert!(n.as_bytes().ends_with(b" (7)"));
        let long_ext = OsString::from_vec([b"a.".as_slice(), &[b'e'; 253]].concat());
        assert_eq!(numbered_name(&long_ext, 1).len(), 255);
    }
}
