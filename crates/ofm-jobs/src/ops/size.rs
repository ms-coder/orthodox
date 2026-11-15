//! Directory size calculation (T3.2 owns this file; F19, "F3 on directory"). Read-only, cancellable
//! (`ctx.checkpoint()` per entry), hardlinks counted once by (dev, ino) when `nlink > 1`, symlinks
//! counted as entries of `lstat` size and never followed, unreadable places are warnings
//! (`ctx.warn`), not errors, and never ask a question. Progress: `ctx.add_total(files, bytes)`
//! while walking (the UI shows a running total), `set_current(dir)`.
//!
//! Semantics of [`SizeTotals`]: `bytes` = sum of `st_size` of non-directory entries; `disk_bytes`
//! = `st_blocks * 512` of every entry including directories (like `du`); `files` = non-directory
//! entries; `dirs` = directories including the roots passed in.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::event::SizeTotals;

/// Total size of `paths`.
pub fn run(ctx: &mut JobCtx, paths: &[PathBuf]) -> Result<SizeTotals, JobError> {
    let fs = ctx.fs();
    let mut totals = SizeTotals::default();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    ctx.set_planning(true);

    for root in paths {
        let mut stack = vec![root.clone()];
        while let Some(path) = stack.pop() {
            ctx.checkpoint()?;
            let st = match fs.lstat(&path) {
                Ok(st) => st,
                Err(e) => {
                    ctx.warn(&path, format!("cannot stat: {e}"));
                    continue;
                }
            };
            if st.nlink > 1 && !st.is_dir() && !seen.insert((st.dev, st.ino)) {
                continue; // another name of an already counted file
            }
            totals.disk_bytes = totals
                .disk_bytes
                .saturating_add(st.blocks.saturating_mul(512));
            if st.is_dir() {
                totals.dirs += 1;
                ctx.set_current(&path);
                match fs.read_dir(&path) {
                    Ok(names) => stack.extend(names.into_iter().map(|n| path.join(n))),
                    Err(e) => ctx.warn(&path, format!("cannot read directory: {e}")),
                }
            } else {
                totals.files += 1;
                totals.bytes = totals.bytes.saturating_add(st.size);
                ctx.add_total(1, st.size);
            }
        }
    }
    ctx.set_planning(false);
    Ok(totals)
}
