//! `run_job`: executes one [`JobSpec`] on the current thread against a [`JobCtx`] and turns the
//! result into an [`Outcome`] (frozen by T3.0). The engine's worker calls it; tests call it (or
//! [`run_sync`]) directly with a `ScriptedSink` - no engine needed.
//!
//! Guarantees: a panic inside an operation is caught and becomes `Outcome::Failed` (invariant 7);
//! `Err(JobError::Cancelled)` becomes `Outcome::Cancelled`; the final exact progress snapshot is
//! emitted before returning; every `todo!()` stub therefore shows up as
//! `Failed { error: "internal error: not yet implemented: T3.x" }` rather than killing a thread.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

use crate::copy::run_copy;
use crate::ctx::{JobCtx, JobSink};
use crate::error::JobError;
use crate::event::{Outcome, SizeTotals};
use crate::extract::run_extract;
use crate::fs::Fs;
use crate::move_::run_move;
use crate::ops;
use crate::rename_batch::{run_batch, run_rollback};
use crate::spec::JobSpec;

fn dispatch(spec: &JobSpec, ctx: &mut JobCtx) -> Result<Option<SizeTotals>, JobError> {
    match spec {
        JobSpec::Copy {
            sources,
            dest_dir,
            opts,
        } => {
            ctx.set_conflict_policy(opts.on_conflict);
            run_copy(ctx, sources, dest_dir, opts)?;
        }
        JobSpec::Move {
            sources,
            dest_dir,
            opts,
        } => {
            ctx.set_conflict_policy(opts.on_conflict);
            run_move(ctx, sources, dest_dir, opts)?;
        }
        JobSpec::Delete { paths, to_trash } => ops::delete::run(ctx, paths, *to_trash)?,
        JobSpec::Mkdir { path } => ops::mkdir::run(ctx, path)?,
        JobSpec::Link { target, link, kind } => ops::link::run(ctx, target, link, *kind)?,
        JobSpec::SetAttrs {
            paths,
            mode,
            owner,
            recursive,
        } => ops::attrs::run(ctx, paths, *mode, *owner, *recursive)?,
        JobSpec::CalcSize { paths } => return ops::size::run(ctx, paths).map(Some),
        JobSpec::Extract {
            archive,
            entries,
            dest_dir,
            opts,
        } => {
            ctx.set_conflict_policy(opts.on_conflict);
            run_extract(ctx, archive, entries, dest_dir, opts)?;
        }
        JobSpec::CompareContent { pairs, opts } => {
            // The results travel through `opts.on_result`; the outcome carries counters only.
            crate::compare::run(ctx, pairs, opts)?;
        }
        JobSpec::RenameBatch(spec) => run_batch(ctx, spec)?,
        JobSpec::RenameRollback(spec) => run_rollback(ctx, spec)?,
    }
    Ok(None)
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// Run `spec` to completion on this thread.
pub fn run_job(spec: &JobSpec, ctx: &mut JobCtx) -> Outcome {
    let result = catch_unwind(AssertUnwindSafe(|| dispatch(spec, ctx)));
    ctx.finish_progress();
    let report = ctx.take_report();
    match result {
        Ok(Ok(None)) => Outcome::Ok(report),
        Ok(Ok(Some(totals))) => Outcome::Size(totals),
        Ok(Err(JobError::Cancelled)) => Outcome::Cancelled(report),
        Ok(Err(e)) => Outcome::Failed {
            error: e.to_string(),
            path: e.path().map(PathBuf::from),
            report,
        },
        Err(payload) => {
            let msg = panic_message(&*payload);
            tracing::error!(panic = %msg, "job panicked");
            Outcome::Failed {
                error: JobError::Panic(msg).to_string(),
                path: None,
                report,
            }
        }
    }
}

/// Test/CLI convenience: run `spec` synchronously with a fresh [`JobCtx::simple`].
pub fn run_sync(spec: &JobSpec, fs: Arc<dyn Fs>, sink: impl JobSink + 'static) -> Outcome {
    let mut ctx = JobCtx::simple(fs, sink);
    run_job(spec, &mut ctx)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::ctx::ScriptedSink;
    use crate::fs::RealFs;

    #[test]
    fn stubs_are_reported_as_failed_not_as_a_dead_thread() {
        let (sink, _) = ScriptedSink::refusing();
        let out = run_sync(&JobSpec::CalcSize { paths: vec![] }, Arc::new(RealFs), sink);
        // Until T3.2 lands this is a caught todo!(); afterwards a real result. Either way no panic
        // escapes and the outcome is well-formed.
        assert!(matches!(out, Outcome::Failed { .. } | Outcome::Size(_)));
    }

    #[test]
    fn unsupported_variants_fail_cleanly() {
        let (sink, _) = ScriptedSink::refusing();
        let out = run_sync(
            &JobSpec::Extract {
                archive: "/x.zip".into(),
                entries: vec![],
                dest_dir: "/y".into(),
                opts: Default::default(),
            },
            Arc::new(RealFs),
            sink,
        );
        assert!(
            matches!(&out, Outcome::Failed { error, .. } if error.contains("not supported")),
            "{out:?}"
        );
    }
}
