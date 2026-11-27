//! The directory tree of the output: creating the chain of parents of every entry (a parent is
//! always checked with `lstat` and created by this job, never followed through a symlink) and the
//! final mode/mtime of the directories the job created. All paths here are nominal ones; the
//! extractor's anchored file system keeps them from being re-resolved through a swapped parent.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_vfs::archive::ArchiveEntry;

use crate::copy::{Target, resolve_target};
use crate::ctx::JobCtx;
use crate::event::Operation;
use crate::fs::FileType;

use super::write::dest_fail;
use super::{Extractor, Fail};

/// Mode of a directory the archive does not describe (synthesised parents).
const DEFAULT_DIR_MODE: u32 = 0o755;

/// One directory of the output tree.
#[derive(Clone)]
pub(crate) struct DirInfo {
    /// Where it really is (differs from the nominal path after "rename new").
    pub(crate) actual: PathBuf,
    /// Created by this job (gets final metadata) rather than merged into an existing one.
    pub(crate) created: bool,
    /// Final permission bits (`& 0o777` when applied).
    pub(crate) mode: u32,
    /// Final mtime in ns; 0 = not known, leave the time the job left it with.
    pub(crate) mtime_ns: i128,
    /// `(dev, ino)` right after the job created it: final metadata goes to that directory only.
    pub(crate) id: Option<(u64, u64)>,
}

/// What [`Extractor::make_dir`] found or created.
struct Made {
    actual: PathBuf,
    created: bool,
    /// Identity of a created directory (`None`: merged, or it could not be queried).
    id: Option<(u64, u64)>,
}

impl Extractor {
    /// The real directory for the relative directory `rel_dir` (empty = the destination itself),
    /// creating missing levels with mode 0700. `Ok(None)` = a level was skipped by the user (it
    /// is remembered, so its whole subtree is skipped without asking again).
    pub(crate) fn parent_dir(
        &mut self,
        ctx: &mut JobCtx,
        rel_dir: &Path,
    ) -> Result<Option<PathBuf>, Fail> {
        let mut cur_rel = PathBuf::new();
        let mut cur = self.dest_dir.clone();
        for comp in rel_dir.components() {
            cur_rel.push(comp);
            if self.blocked.contains(&cur_rel) {
                return Ok(None);
            }
            if let Some(d) = self.dirs.get(&cur_rel) {
                cur = d.actual.clone();
                continue;
            }
            let nominal = cur.join(comp);
            let made = match self.make_dir(ctx, &nominal, &cur_rel) {
                Ok(m) => m,
                // One warning for the directory; its subtree is skipped without further tries.
                Err(f @ Fail::Entry { .. }) => {
                    self.blocked.insert(cur_rel);
                    return Err(f);
                }
                Err(f) => return Err(f),
            };
            let Some(Made {
                actual,
                created,
                id,
            }) = made
            else {
                self.blocked.insert(cur_rel);
                return Ok(None);
            };
            if cur_rel.components().count() == 1 {
                ctx.note_created(&actual);
            }
            self.dirs.insert(
                cur_rel.clone(),
                DirInfo {
                    actual: actual.clone(),
                    created,
                    mode: DEFAULT_DIR_MODE,
                    mtime_ns: 0,
                    id,
                },
            );
            cur = actual;
        }
        Ok(Some(cur))
    }

    /// Create (or merge into) the directory `nominal`; asks on a conflict like copy does.
    fn make_dir(
        &mut self,
        ctx: &mut JobCtx,
        nominal: &Path,
        rel: &Path,
    ) -> Result<Option<Made>, Fail> {
        let st = self.fake_stat(FileType::Dir, 0o755, 0, 0);
        let src = self.archive.join(rel);
        // One handle of the parent for the probe, the question and the mkdir.
        let _parent = match nominal.parent() {
            Some(p) => Some(self.anchor.hold(p).map_err(|e| dest_fail(nominal, e))?),
            None => None,
        };
        loop {
            let target = resolve_target(
                ctx,
                &src,
                &st,
                nominal,
                Operation::Mkdir,
                self.copier.published(),
            )?;
            match target {
                Target::Skip => {
                    ctx.note_skipped();
                    return Ok(None);
                }
                Target::Merge(p) => {
                    return Ok(Some(Made {
                        actual: p,
                        created: false,
                        id: None,
                    }));
                }
                // Never produced for a directory (resolve_target refuses overwrite across types).
                Target::Replace(_) => {
                    ctx.warn(nominal, "overwrite not allowed for a directory; skipped");
                    ctx.note_skipped();
                    return Ok(None);
                }
                Target::Fresh(p) => match self.fs.mkdir(&p, 0o700) {
                    Ok(()) => {
                        let id = self.note_new_dir(ctx, &p);
                        return Ok(Some(Made {
                            actual: p,
                            created: true,
                            id,
                        }));
                    }
                    // The name appeared since the probe: resolve again.
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(dest_fail(&p, e)),
                },
            }
        }
    }

    /// Record a directory this job created: its identity and name are never overwritable later.
    fn note_new_dir(&mut self, ctx: &mut JobCtx, dir: &Path) -> Option<(u64, u64)> {
        let id = match self.fs.lstat(dir) {
            Ok(st) => {
                self.copier.note_published((st.dev, st.ino));
                Some((st.dev, st.ino))
            }
            Err(e) => {
                tracing::debug!(path = %dir.display(), error = %e, "lstat new dir");
                None
            }
        };
        self.copier.note_published_path(dir);
        if self.sync
            && let Some(parent) = dir.parent()
            && let Err(e) = self.fs.sync_dir(parent)
        {
            self.meta_warn(ctx, parent, "directory not synced", &e);
        }
        id
    }

    /// A directory entry of the archive: make sure it exists and remember its final metadata.
    pub(crate) fn do_dir(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        rel: &Path,
    ) -> Result<(), Fail> {
        ctx.set_current(&self.dest_dir.join(rel));
        if self.parent_dir(ctx, rel)?.is_none() {
            return Ok(());
        }
        if let Some(d) = self.dirs.get_mut(rel)
            && d.created
        {
            d.mode = e.mode;
            d.mtime_ns = e.mtime_ns;
        }
        self.succeeded();
        Ok(())
    }

    /// Final mode and mtime of the directories this job created, deepest first, so that a
    /// directory that became read-only (or unsearchable) only does so after its contents are in
    /// place. Failures are warnings. Runs after cancel/failure too.
    ///
    /// A directory whose handle left the cache is opened again by name, so another directory may
    /// be found there: metadata is applied only if its `(dev, ino)` is the one recorded at
    /// creation. Where inode numbers are unstable (vfat) a false mismatch only skips metadata.
    pub(crate) fn finish_dirs(&mut self, ctx: &mut JobCtx) {
        let mut list: Vec<(usize, DirInfo)> = self
            .dirs
            .iter()
            .filter(|(_, d)| d.created)
            .map(|(rel, d)| (rel.components().count(), d.clone()))
            .collect();
        list.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.actual.cmp(&b.1.actual)));
        let fs = Arc::clone(&self.fs);
        for (_, d) in list {
            let (path, mode, mtime_ns, id) = (d.actual, d.mode, d.mtime_ns, d.id);
            // One handle for all steps, and only ever a directory.
            let dir = match self.anchor.hold(&path) {
                Ok(d) => d,
                Err(e) => {
                    self.meta_warn(ctx, &path, "permissions not set", &e);
                    continue;
                }
            };
            // Sync first: after the final mode the directory may no longer be openable.
            if self.sync
                && let Err(e) = fs.sync_dir(&path)
            {
                self.meta_warn(ctx, &path, "directory not synced", &e);
            }
            let found = dir.stat().ok().map(|st| (st.dev, st.ino));
            if id.is_none() || found != id {
                let why = std::io::Error::other(
                    "not verifiably the directory this job created (replaced during extraction?)",
                );
                self.meta_warn(ctx, &path, "permissions and times not set", &why);
                continue;
            }
            // Never setuid/setgid/sticky, never beyond rwx for everyone.
            if let Err(e) = fs.set_mode(&path, mode & 0o777) {
                self.meta_warn(ctx, &path, "permissions not set", &e);
            }
            if mtime_ns != 0
                && let Err(e) = fs.set_times(&path, mtime_ns, mtime_ns, false)
            {
                self.meta_warn(ctx, &path, "timestamps not set", &e);
            }
        }
    }
}
