# ADR-0006: Jobs API (question protocol, scheduling, progress, test seams)

Status: accepted · 2026-11-15 · refines ADR-0004 (algorithm unchanged) and ARCHITECTURE §2 `ofm-jobs`

## Context

M3 builds the engine (T3.1), the other operations (T3.2), the UI (T3.3) and the torture suite (T3.4)
in parallel. They need one frozen contract: how a worker asks the user things, how jobs are
scheduled, how progress is reported without ever blocking a worker, and how every operation is tested
without the engine and without a UI.

## Decision

**Layering.** `JobEngine` (threads, queue, events) -> `runner::run_job(spec, &mut JobCtx) -> Outcome`
-> operation (`copy`, `move_`, `ops::*`) -> `JobCtx` -> `Fs`. Operations take `&mut JobCtx` and never see
the engine. `run_job` catches panics (`Outcome::Failed`, invariant 7) and maps
`JobError::Cancelled` to `Outcome::Cancelled`. `runner::run_sync` + `ScriptedSink` + `FaultyFs` let any
operation be tested synchronously, headless, with injected answers and faults.

**`JobCtx`** is the only thing an operation needs: `fs()`, `checkpoint()` (pause point + cancel; call
between items and every 1 MiB), progress counters (`add_bytes`, `file_done`, `set_current`,
`add_total`/`set_totals`), the report (`warn`, `note_created`), and the question helpers below.

**Question protocol.** Worker asks through `JobSink::ask`; the engine's sink sends
`JobEvent::Ask { id, question, reply: Responder<Answer> }` and blocks on the responder, polling the
cancel token every 100 ms (`ask_over_events`), so cancelling a job that waits for an unanswered
dialog works; a dropped responder = abort. Questions: `Conflict { source, existing, kind }`,
`Error { path, operation, message, os_error }`, `TrashUnavailable { path }`. Answers: `Conflict
{ action, apply_to_all }`, `Error { action, apply_to_all }`, `Confirm { permanent, apply_to_all }`; an
answer of the wrong variant counts as abort.
- Conflict actions: `Overwrite, Skip, OverwriteIfNewer, OverwriteIfSizeDiffers, RenameNew,
  RenameExisting, Abort`. "Compare" (F16) is UI-only (it shows both sides and re-asks). Conditional
  answers are evaluated centrally in `JobCtx::resolve_conflict` (source mtime strictly newer / size
  differs, else skip). **Append/resume is not offered in v1.0**: it contradicts "never write into a
  destination in place" (ADR-0004).
- `ConflictKind::{Replaceable, TypeMismatch, SameFile}` decides which answers are allowed:
  `Overwrite*` is refused for file-vs-directory and for same-file (copy into its own directory, F13);
  a forbidden answer degrades to skip + warning, and a remembered "apply to all" is not reused for a
  conflict it does not allow (it asks again). Directory-vs-directory is never a question: it merges.
- `RenameNew` and `RenameExisting` use one scheme: first free `name (N).ext`, N >= 1 (`.bashrc` ->
  `.bashrc (1)`, `a.tar.gz` -> `a.tar (1).gz`, directories `dir (1)`), probed with `lstat`.
- Errors: every fallible syscall of an operation goes through `JobCtx::attempt` (retry/skip/abort);
  "apply to all" is remembered for skip/abort only (a remembered retry would loop forever). Abort or
  no answer = `Err(Cancelled)` = same cleanup as a user cancel.
- `Answer::Confirm` for the trash question: `permanent == false` skips the item.

**Scheduling.** One FIFO queue lane thread for `RunMode::Queued` (default: one disk-heavy job at a
time), one thread per `RunMode::Parallel` job. Per job events are ordered `Queued -> Started ->
(Progress | Ask | StateChanged)* -> Finished` (`Queued` is emitted before `submit` returns). The event
receiver is a work queue: exactly one consumer (`JobsController`). Cancel of a queued job finishes it
as `Cancelled` without `Started`. Dropping the engine cancels all jobs and joins the workers.

**Progress cadence.** `ProgressTracker` accumulates; `JobCtx` hands a snapshot to the sink at most every
100 ms (10 Hz; the first one immediately, the final exact one always before `Finished`). The sink must
not block: the engine uses an unbounded channel (<= 10 events/s/job). Speed is an EMA over >= 100 ms
windows (alpha 0.3), reset after a pause; ETA only after planning. The clock is injected (`Clock`,
`ManualClock` in tests). While scanning, `Progress.planning == true` and totals grow.

**Results carry paths.** `Outcome::{Ok(Report), Cancelled(Report), Failed { error, path, report },
Size(SizeTotals)}`; `Report` has warnings (path + message), skipped count, files/bytes done and
`created` (final top-level destination paths: the UI focuses the first one after the refresh).
`JobError` variants carry their path; `Unsupported` covers archive sources (M6) and batch rename (M8).

**`Fs` seam.** `FsFile::as_fd()` exposes the descriptor so `try_clone_from` / `copy_range_from` accept
`&dyn FsFile` sources, including wrapped ones: `FaultyFs` intercepts every `Fs`/`FsFile` method
(`FaultOp::TryCloneFrom`, `CopyRangeFrom`, new `Trash`). The trash goes through `Fs::trash` so tests can
fault it. `FaultyFs` lives in `ofm-jobs` behind the `testing` feature (it needs `ofm-testkit`'s
`FaultPlan`; the testkit stays a dev-only dependency, `ofm-jobs`'s own tests enable the feature via a
self dev-dependency). `RealFs` is built on `rustix` (no `unsafe`, no libc crate).

**Other decisions.** Rename in place (Shift+F6) is a `Move` into the same directory with a literal rename
mask; `JobSpec::Rename` is only the F20 batch rename (M8). v1.0/M3 copy and move sources are
`Location::Local` only (other locations are `Unsupported` until M6/M7 add a source-reader seam).
Panels refresh through the directory watcher (`PanelController`, T3.0), not through job events;
`Report::created` + `PanelController::focus_name` only place the cursor.

## Alternatives considered

- A bounded or "latest value" progress slot: more machinery, and an unbounded channel at 10 Hz is cheap.
- Letting each operation talk to the UI channel directly: untestable without the engine.
- Making `Fs` generic instead of `dyn`: infects every signature; the virtual call cost is nothing next
  to a syscall.
- `FaultyFs` in `ofm-testkit`: would make the testkit depend on `ofm-jobs` (cycle with its dev use).

## Consequences

- T3.1/T3.2/T3.4 only touch their own files; the frozen files change through the architect.
- Every new operation gets cancel/pause/ask/progress/report for free by using `JobCtx`.
- The question protocol is a UI contract: `ConflictKind::allows` must be honoured by the dialog.
